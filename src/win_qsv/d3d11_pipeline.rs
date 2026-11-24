//! Shared D3D11 device management, NV12 surface pool, GPU RGBA -> NV12 converters
//! (D3D11 Compute Shader + D3D11 Video Processor), and D3D12 <-> D3D11 shared ring.

use super::{
    ColorMatrix, ColorRange, Nv12ConverterMode, WinGpuFrame, mfx_ffi::MfxHdlPair,
    record_pool_acquire, record_pool_capacity, record_pool_release,
};
use anyhow::{Context, Result, bail};
use skia_safe::gpu::{
    self, DirectContext, FlushInfo, SurfaceOrigin, backend_render_targets, d3d as skia_d3d,
};
use skia_safe::surface::BackendSurfaceAccess;
use skia_safe::{ColorSpace, ColorType, Surface};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE, LUID, RECT};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SRV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{Interface, PCSTR};

pub const INTEL_VENDOR_ID: u32 = 0x8086;
pub const RGBA_RING_SIZE: usize = 3;
pub const DEFAULT_NV12_POOL_SIZE: usize = 12;
const GPU_DRAIN_TIMEOUT_MS: u32 = 5_000;

fn luid_key(luid: LUID) -> (i32, u32) {
    (luid.HighPart, luid.LowPart)
}

/// RAII guard combining `ctx_lock` (Goosy threads) and `ID3D11Multithread::Enter/Leave`
/// (mutual exclusion with Intel MFX's internal D3D11 immediate-context operations).
pub struct D3d11ImmediateContextGuard<'a> {
    _mutex_guard: MutexGuard<'a, ()>,
    multithread: &'a ID3D11Multithread,
}

impl Drop for D3d11ImmediateContextGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            self.multithread.Leave();
        }
    }
}

/// Thread-safe, multithread-protected D3D11 device shared between D3D12 interop,
/// GPU RGBA->NV12 conversion, and Intel QSV (`MFX_HANDLE_D3D11_DEVICE`).
pub struct SharedD3d11Context {
    pub device: ID3D11Device,
    pub device1: ID3D11Device1,
    pub device5: ID3D11Device5,
    pub context: ID3D11DeviceContext,
    pub context4: ID3D11DeviceContext4,
    pub multithread: ID3D11Multithread,
    pub video_device: Option<ID3D11VideoDevice>,
    pub video_context: Option<ID3D11VideoContext>,
    pub video_context1: Option<ID3D11VideoContext1>,
    pub adapter_luid: LUID,
    pub adapter_name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub supports_direct_nv12_uav: bool,
    pub supports_nv12_rtv: bool,
    pub supports_vp_rgba_to_nv12: bool,
    /// Serializes immediate-context submissions from render/convert & CPU upload fallback.
    pub ctx_lock: Mutex<()>,
}

// D3D11 interfaces with ID3D11Multithread::SetMultithreadProtected(true) are thread-safe.
unsafe impl Send for SharedD3d11Context {}
unsafe impl Sync for SharedD3d11Context {}

static CONTEXT_REGISTRY: Mutex<Vec<((i32, u32), Weak<SharedD3d11Context>)>> = Mutex::new(Vec::new());

impl SharedD3d11Context {
    /// Acquires exclusive access to the D3D11 immediate context across both Goosy
    /// threads and Intel MFX internal threads.
    pub fn lock_immediate_context(&self) -> D3d11ImmediateContextGuard<'_> {
        let guard = self.ctx_lock.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            self.multithread.Enter();
        }
        D3d11ImmediateContextGuard {
            _mutex_guard: guard,
            multithread: &self.multithread,
        }
    }

    /// Returns an existing healthy shared D3D11 context for `luid`, or creates a new one on that adapter.
    pub fn get_or_create_for_luid(luid: LUID) -> Result<Arc<Self>> {
        let key = luid_key(luid);
        {
            let mut reg = CONTEXT_REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
            reg.retain(|(_, w)| w.upgrade().is_some_and(|c| c.check_device_health().is_ok()));
            if let Some((_, weak)) = reg.iter().find(|(k, _)| *k == key)
                && let Some(existing) = weak.upgrade()
            {
                return Ok(existing);
            }
        }
        let adapter = find_adapter_by_luid(luid)
            .with_context(|| format!("locate DXGI adapter for LUID {:08X}:{:08X}", luid.HighPart, luid.LowPart))?;
        let created = Arc::new(Self::create_on_adapter(&adapter)?);
        let mut reg = CONTEXT_REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        reg.retain(|(_, w)| w.upgrade().is_some_and(|c| c.check_device_health().is_ok()));
        // Re-check after re-acquiring lock to prevent TOCTOU duplicate device creation.
        if let Some((_, weak)) = reg.iter().find(|(k, _)| *k == key)
            && let Some(existing) = weak.upgrade()
        {
            return Ok(existing);
        }
        reg.push((key, Arc::downgrade(&created)));
        Ok(created)
    }

    /// Returns an already-registered healthy Intel D3D11 context (e.g. from `Direct3DBackend`),
    /// or creates one on the first Intel hardware DXGI adapter.
    pub fn get_or_create_intel() -> Result<Arc<Self>> {
        {
            let mut reg = CONTEXT_REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
            reg.retain(|(_, w)| w.upgrade().is_some_and(|c| c.check_device_health().is_ok()));
            for (_, weak) in reg.iter() {
                if let Some(ctx) = weak.upgrade()
                    && ctx.vendor_id == INTEL_VENDOR_ID
                {
                    return Ok(ctx);
                }
            }
        }
        let adapter = find_intel_adapter().context("find an Intel DXGI hardware adapter")?;
        let desc = unsafe { adapter.GetDesc1()? };
        Self::get_or_create_for_luid(desc.AdapterLuid)
    }

    fn create_on_adapter(adapter: &IDXGIAdapter1) -> Result<Self> {
        let desc = unsafe { adapter.GetDesc1()? };
        let adapter_name = String::from_utf16_lossy(
            &desc.Description[..desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len())],
        );

        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .with_context(|| format!("D3D11CreateDevice(VIDEO_SUPPORT) on {adapter_name}"))?;
        }
        let device = device.context("D3D11CreateDevice returned null device")?;
        let context = context.context("D3D11CreateDevice returned null context")?;
        let multithread: ID3D11Multithread =
            device.cast().context("query ID3D11Multithread")?;
        unsafe {
            let _ = multithread.SetMultithreadProtected(true);
        }

        let device1: ID3D11Device1 = device.cast().context("query ID3D11Device1")?;
        let device5: ID3D11Device5 = device.cast().context("query ID3D11Device5")?;
        let context4: ID3D11DeviceContext4 = context.cast().context("query ID3D11DeviceContext4")?;
        let video_device: Option<ID3D11VideoDevice> = device.cast().ok();
        let video_context: Option<ID3D11VideoContext> = context.cast().ok();
        let video_context1: Option<ID3D11VideoContext1> = context.cast().ok();

        let (supports_direct_nv12_uav, supports_nv12_rtv) = probe_nv12_uav_and_rtv(&device);
        let supports_vp_rgba_to_nv12 = video_device
            .as_ref()
            .is_some_and(probe_video_processor_rgba_to_nv12);

        Ok(Self {
            device,
            device1,
            device5,
            context,
            context4,
            multithread,
            video_device,
            video_context,
            video_context1,
            adapter_luid: desc.AdapterLuid,
            adapter_name,
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
            supports_direct_nv12_uav,
            supports_nv12_rtv,
            supports_vp_rgba_to_nv12,
            ctx_lock: Mutex::new(()),
        })
    }

    pub fn check_device_health(&self) -> Result<()> {
        let reason = unsafe { self.device.GetDeviceRemovedReason() };
        if let Err(err) = reason {
            bail!(
                "D3D11 device lost on {} (reason: {err})",
                self.adapter_name
            );
        }
        Ok(())
    }

    pub fn resolve_converter_mode(&self, requested: Nv12ConverterMode) -> Result<Nv12ConverterMode> {
        match requested {
            Nv12ConverterMode::Auto => {
                // On Intel iGPUs, D3D11 Video Processor executes on dedicated VEBOX/SFC
                // fixed-function silicon, avoiding 3D/EU contention with Skia rendering.
                // Fall back to direct NV12 UAV Compute Shader, then intermediate UAV+RTV.
                if self.vendor_id == INTEL_VENDOR_ID && self.supports_vp_rgba_to_nv12 {
                    Ok(Nv12ConverterMode::VideoProcessor)
                } else if self.supports_direct_nv12_uav {
                    Ok(Nv12ConverterMode::ComputeDirect)
                } else if self.supports_vp_rgba_to_nv12 {
                    Ok(Nv12ConverterMode::VideoProcessor)
                } else if self.supports_nv12_rtv {
                    Ok(Nv12ConverterMode::ComputeIntermediate)
                } else {
                    bail!(
                        "D3D11 device {} supports neither NV12 UAV, NV12 RTV, nor Video Processor",
                        self.adapter_name
                    );
                }
            }
            Nv12ConverterMode::ComputeDirect => {
                if !self.supports_direct_nv12_uav {
                    bail!(
                        "direct NV12 UAV compute mode is unsupported on {}",
                        self.adapter_name
                    );
                }
                Ok(Nv12ConverterMode::ComputeDirect)
            }
            Nv12ConverterMode::ComputeIntermediate => {
                if !self.supports_nv12_rtv {
                    bail!(
                        "intermediate NV12 RTV compute mode is unsupported on {}",
                        self.adapter_name
                    );
                }
                Ok(Nv12ConverterMode::ComputeIntermediate)
            }
            Nv12ConverterMode::VideoProcessor => {
                if !self.supports_vp_rgba_to_nv12 {
                    bail!(
                        "D3D11 Video Processor RGBA->NV12 is unsupported on {}",
                        self.adapter_name
                    );
                }
                Ok(Nv12ConverterMode::VideoProcessor)
            }
        }
    }
}

fn find_adapter_by_luid(luid: LUID) -> Result<IDXGIAdapter1> {
    let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1()? };
    unsafe { factory.EnumAdapterByLuid(luid) }.context("IDXGIFactory4::EnumAdapterByLuid")
}

fn find_intel_adapter() -> Result<IDXGIAdapter1> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        let flags = DXGI_ADAPTER_FLAG(desc.Flags as i32);
        if flags.contains(DXGI_ADAPTER_FLAG_SOFTWARE) || flags.contains(DXGI_ADAPTER_FLAG_REMOTE) {
            continue;
        }
        if desc.VendorId == INTEL_VENDOR_ID {
            return Ok(adapter);
        }
    }
    bail!("no Intel hardware DXGI adapter found")
}

fn probe_nv12_uav_and_rtv(device: &ID3D11Device) -> (bool, bool) {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: 64,
        Height: 64,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0
            | D3D11_BIND_UNORDERED_ACCESS.0
            | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    let uav_ok = if unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.is_ok() {
        let t = tex.unwrap();
        let y_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R8_UNORM,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 },
            },
        };
        let uv_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8_UNORM,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 },
            },
        };
        let mut uy = None;
        let mut uuv = None;
        unsafe {
            device
                .CreateUnorderedAccessView(&t, Some(&y_uav_desc), Some(&mut uy))
                .is_ok()
                && device
                    .CreateUnorderedAccessView(&t, Some(&uv_uav_desc), Some(&mut uuv))
                    .is_ok()
        }
    } else {
        false
    };

    let mut rtv_desc = desc;
    rtv_desc.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
    let mut rtex: Option<ID3D11Texture2D> = None;
    let rtv_ok = if unsafe { device.CreateTexture2D(&rtv_desc, None, Some(&mut rtex)) }.is_ok() {
        let t = rtex.unwrap();
        let y_rtv = D3D11_RENDER_TARGET_VIEW_DESC {
            Format: DXGI_FORMAT_R8_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
            },
        };
        let uv_rtv = D3D11_RENDER_TARGET_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
            },
        };
        let mut ry = None;
        let mut ruv = None;
        unsafe {
            device
                .CreateRenderTargetView(&t, Some(&y_rtv), Some(&mut ry))
                .is_ok()
                && device
                    .CreateRenderTargetView(&t, Some(&uv_rtv), Some(&mut ruv))
                    .is_ok()
        }
    } else {
        false
    };

    (uav_ok, rtv_ok)
}

fn probe_video_processor_rgba_to_nv12(video_device: &ID3D11VideoDevice) -> bool {
    let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        },
        InputWidth: 64,
        InputHeight: 64,
        OutputFrameRate: DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        },
        OutputWidth: 64,
        OutputHeight: 64,
        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    };
    let Ok(enumerator) = (unsafe { video_device.CreateVideoProcessorEnumerator(&content) }) else {
        return false;
    };
    let in_ok = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_R8G8B8A8_UNORM) }
        .is_ok_and(|f| f & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0);
    let out_ok = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
        .is_ok_and(|f| f & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 != 0);
    in_ok && out_ok
}

// ============================================================================
// Pooled NV12 GPU Surface Allocation (Zero-Copy Input to QSV)
// ============================================================================

pub struct Nv12PoolSlot {
    pub texture: ID3D11Texture2D,
    pub uav_y: Option<ID3D11UnorderedAccessView>,
    pub uav_uv: Option<ID3D11UnorderedAccessView>,
    pub rtv_y: Option<ID3D11RenderTargetView>,
    pub rtv_uv: Option<ID3D11RenderTargetView>,
    pub vp_out_view: Option<ID3D11VideoProcessorOutputView>,
    /// Stable heap-allocated `mfxHDLPair { ID3D11Texture2D*, 0 }` for zero-copy QSV `MemId`.
    pub hdl_pair: Box<MfxHdlPair>,
}

unsafe impl Send for Nv12PoolSlot {}
unsafe impl Sync for Nv12PoolSlot {}

pub struct Nv12SurfacePool {
    pub d3d11: Arc<SharedD3d11Context>,
    pub width: u32,
    pub height: u32,
    pub aligned_width: u32,
    pub aligned_height: u32,
    slots: Vec<Nv12PoolSlot>,
    free_list: Mutex<VecDeque<usize>>,
    free_cv: Condvar,
}

unsafe impl Send for Nv12SurfacePool {}
unsafe impl Sync for Nv12SurfacePool {}

pub struct Nv12SurfaceLease {
    pool: Arc<Nv12SurfacePool>,
    index: usize,
}

unsafe impl Send for Nv12SurfaceLease {}
unsafe impl Sync for Nv12SurfaceLease {}

impl Nv12SurfaceLease {
    pub fn slot(&self) -> &Nv12PoolSlot {
        &self.pool.slots[self.index]
    }

    pub fn d3d11(&self) -> &Arc<SharedD3d11Context> {
        &self.pool.d3d11
    }

    pub fn mem_id(&self) -> *mut c_void {
        &*self.slot().hdl_pair as *const MfxHdlPair as *mut c_void
    }

    pub fn slot_index(&self) -> usize {
        self.index
    }
}

impl Drop for Nv12SurfaceLease {
    fn drop(&mut self) {
        let mut free = self.pool.free_list.lock().unwrap();
        free.push_back(self.index);
        record_pool_release();
        self.pool.free_cv.notify_one();
    }
}

impl Nv12SurfacePool {
    pub fn new(
        d3d11: Arc<SharedD3d11Context>,
        width: u32,
        height: u32,
        pool_size: usize,
        vp_enum: Option<&ID3D11VideoProcessorEnumerator>,
    ) -> Result<Arc<Self>> {
        let mode = if vp_enum.is_some() {
            Nv12ConverterMode::VideoProcessor
        } else {
            Nv12ConverterMode::Auto
        };
        Self::new_for_mode(d3d11, width, height, pool_size, mode, vp_enum)
    }

    pub fn new_for_mode(
        d3d11: Arc<SharedD3d11Context>,
        width: u32,
        height: u32,
        pool_size: usize,
        mode: Nv12ConverterMode,
        vp_enum: Option<&ID3D11VideoProcessorEnumerator>,
    ) -> Result<Arc<Self>> {
        let aligned_width = width.next_multiple_of(16);
        let aligned_height = height.next_multiple_of(16);
        let need_uav = matches!(mode, Nv12ConverterMode::ComputeDirect)
            || (mode == Nv12ConverterMode::Auto && d3d11.supports_direct_nv12_uav);
        let need_rtv = matches!(mode, Nv12ConverterMode::ComputeIntermediate)
            || (mode == Nv12ConverterMode::Auto && d3d11.supports_nv12_rtv);
        let need_vp = matches!(mode, Nv12ConverterMode::VideoProcessor);

        let bind_flags = if need_uav {
            (D3D11_BIND_RENDER_TARGET.0
                | D3D11_BIND_UNORDERED_ACCESS.0
                | D3D11_BIND_SHADER_RESOURCE.0) as u32
        } else {
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32
        };

        let mut slots = Vec::with_capacity(pool_size);
        let mut free_list = VecDeque::with_capacity(pool_size);

        for idx in 0..pool_size {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: aligned_width,
                Height: aligned_height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: bind_flags,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            unsafe {
                d3d11
                    .device
                    .CreateTexture2D(&desc, None, Some(&mut tex))
                    .with_context(|| {
                        format!(
                            "allocate pooled NV12 D3D11 texture #{idx} ({aligned_width}x{aligned_height})"
                        )
                    })?;
            }
            let texture = tex.unwrap();

            let (uav_y, uav_uv) = if need_uav {
                let y_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_R8_UNORM,
                    ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 },
                    },
                };
                let uv_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_R8G8_UNORM,
                    ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 },
                    },
                };
                let mut uy = None;
                let mut uuv = None;
                unsafe {
                    d3d11
                        .device
                        .CreateUnorderedAccessView(&texture, Some(&y_desc), Some(&mut uy))
                        .context("create NV12 Y UAV")?;
                    d3d11
                        .device
                        .CreateUnorderedAccessView(&texture, Some(&uv_desc), Some(&mut uuv))
                        .context("create NV12 UV UAV")?;
                }
                (uy, uuv)
            } else {
                (None, None)
            };

            let (rtv_y, rtv_uv) = if need_rtv {
                let y_desc = D3D11_RENDER_TARGET_VIEW_DESC {
                    Format: DXGI_FORMAT_R8_UNORM,
                    ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
                    },
                };
                let uv_desc = D3D11_RENDER_TARGET_VIEW_DESC {
                    Format: DXGI_FORMAT_R8G8_UNORM,
                    ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
                    },
                };
                let mut ry = None;
                let mut ruv = None;
                let r1 = unsafe {
                    d3d11
                        .device
                        .CreateRenderTargetView(&texture, Some(&y_desc), Some(&mut ry))
                };
                let r2 = unsafe {
                    d3d11
                        .device
                        .CreateRenderTargetView(&texture, Some(&uv_desc), Some(&mut ruv))
                };
                if matches!(mode, Nv12ConverterMode::ComputeIntermediate) {
                    r1.context("create NV12 Y RTV")?;
                    r2.context("create NV12 UV RTV")?;
                }
                (ry, ruv)
            } else {
                (None, None)
            };

            let vp_out_view = if let (Some(vdev), Some(vpenum)) = (&d3d11.video_device, vp_enum) {
                let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                    },
                };
                let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
                let res = unsafe {
                    vdev.CreateVideoProcessorOutputView(
                        &texture,
                        vpenum,
                        &out_desc,
                        Some(&mut out_view),
                    )
                };
                if need_vp {
                    res.with_context(|| format!("create NV12 VP output view #{idx}"))?;
                }
                out_view
            } else if need_vp {
                bail!("VideoProcessor mode requested without ID3D11VideoDevice / enumerator");
            } else {
                None
            };

            let hdl_pair = Box::new(MfxHdlPair {
                first: texture.as_raw(),
                second: null_mut(),
            });

            slots.push(Nv12PoolSlot {
                texture,
                uav_y,
                uav_uv,
                rtv_y,
                rtv_uv,
                vp_out_view,
                hdl_pair,
            });
            free_list.push_back(idx);
        }

        record_pool_capacity(pool_size);
        Ok(Arc::new(Self {
            d3d11,
            width,
            height,
            aligned_width,
            aligned_height,
            slots,
            free_list: Mutex::new(free_list),
            free_cv: Condvar::new(),
        }))
    }

    pub fn acquire(self: &Arc<Self>) -> Nv12SurfaceLease {
        let mut free = self.free_list.lock().unwrap();
        let index = loop {
            if let Some(i) = free.pop_front() {
                break i;
            }
            free = self.free_cv.wait(free).unwrap();
        };
        record_pool_acquire();
        Nv12SurfaceLease {
            pool: Arc::clone(self),
            index,
        }
    }
}

// ============================================================================
// GPU RGBA -> NV12 Converter (Compute Shader Direct, Compute Staged, VideoProcessor)
// ============================================================================

#[repr(C)]
#[derive(Clone, Copy)]
struct YuvColorParams {
    y_coeff: [f32; 4],  // rgb + y_offset
    u_coeff: [f32; 4],  // rgb + uv_offset
    v_coeff: [f32; 4],  // rgb + unused
    dims: [u32; 4],     // width, height, aligned_width, aligned_height
}

fn color_params(
    matrix: ColorMatrix,
    range: ColorRange,
    width: u32,
    height: u32,
    aligned_width: u32,
    aligned_height: u32,
) -> YuvColorParams {
    let (kr, kg, kb) = match matrix {
        ColorMatrix::Bt601 => (0.299f32, 0.587f32, 0.114f32),
        ColorMatrix::Bt709 => (0.2126f32, 0.7152f32, 0.0722f32),
        ColorMatrix::Bt2020 => (0.2627f32, 0.6780f32, 0.0593f32),
    };
    let (y_scale, uv_scale, y_offset) = match range {
        ColorRange::Limited => (219.0 / 255.0, 224.0 / 255.0, 16.0 / 255.0),
        ColorRange::Full => (1.0, 1.0, 0.0),
    };
    let uv_offset = 128.0 / 255.0;

    let yr = kr * y_scale;
    let yg = kg * y_scale;
    let yb = kb * y_scale;

    let u_norm = 0.5 * uv_scale / (1.0 - kb);
    let ur = -kr * u_norm;
    let ug = -kg * u_norm;
    let ub = (1.0 - kb) * u_norm;

    let v_norm = 0.5 * uv_scale / (1.0 - kr);
    let vr = (1.0 - kr) * v_norm;
    let vg = -kg * v_norm;
    let vb = -kb * v_norm;

    YuvColorParams {
        y_coeff: [yr, yg, yb, y_offset],
        u_coeff: [ur, ug, ub, uv_offset],
        v_coeff: [vr, vg, vb, 0.0],
        dims: [width, height, aligned_width, aligned_height],
    }
}

const HLSL_RGBA_TO_NV12_CS: &str = r#"
Texture2D<float4> srcTex : register(t0);
RWTexture2D<unorm float> dstY : register(u0);
RWTexture2D<unorm float2> dstUV : register(u1);

cbuffer YuvParams : register(b0) {
    float4 y_coeff; // xyz = rgb weights, w = y_offset
    float4 u_coeff; // xyz = rgb weights, w = uv_offset
    float4 v_coeff; // xyz = rgb weights
    uint4  dims;    // x = width, y = height, z = aligned_w, w = aligned_h
};

[numthreads(16, 16, 1)]
void main(uint3 gid : SV_DispatchThreadID) {
    uint x = gid.x * 2;
    uint y = gid.y * 2;
    if (x >= dims.z || y >= dims.w) return;

    uint max_x = dims.x - 1;
    uint max_y = dims.y - 1;

    uint sx0 = min(x, max_x);
    uint sx1 = min(x + 1, max_x);
    uint sy0 = min(y, max_y);
    uint sy1 = min(y + 1, max_y);

    float3 p00 = srcTex.Load(int3(sx0, sy0, 0)).rgb;
    float3 p10 = srcTex.Load(int3(sx1, sy0, 0)).rgb;
    float3 p01 = srcTex.Load(int3(sx0, sy1, 0)).rgb;
    float3 p11 = srcTex.Load(int3(sx1, sy1, 0)).rgb;

    dstY[uint2(x, y)]         = saturate(dot(p00, y_coeff.xyz) + y_coeff.w);
    dstY[uint2(x + 1, y)]     = saturate(dot(p10, y_coeff.xyz) + y_coeff.w);
    dstY[uint2(x, y + 1)]     = saturate(dot(p01, y_coeff.xyz) + y_coeff.w);
    dstY[uint2(x + 1, y + 1)] = saturate(dot(p11, y_coeff.xyz) + y_coeff.w);

    float3 avg = 0.25 * (p00 + p10 + p01 + p11);
    float u = saturate(dot(avg, u_coeff.xyz) + u_coeff.w);
    float v = saturate(dot(avg, v_coeff.xyz) + u_coeff.w);
    dstUV[gid.xy] = float2(u, v);
}
"#;

const HLSL_BLIT_VS: &str = r#"
struct VSOut {
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};
VSOut main(uint vid : SV_VertexID) {
    VSOut o;
    float2 p = float2((vid << 1) & 2, vid & 2);
    o.uv = p;
    o.pos = float4(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    return o;
}
"#;

const HLSL_BLIT_Y_PS: &str = r#"
Texture2D<float> srcY : register(t0);
float main(float4 pos : SV_Position) : SV_Target0 {
    return srcY.Load(int3(int2(pos.xy), 0));
}
"#;

const HLSL_BLIT_UV_PS: &str = r#"
Texture2D<float2> srcUV : register(t0);
float2 main(float4 pos : SV_Position) : SV_Target0 {
    return srcUV.Load(int3(int2(pos.xy), 0));
}
"#;

fn compile_hlsl(src: &str, entry: &str, target: &str) -> Result<Vec<u8>> {
    let entry_c = std::ffi::CString::new(entry).unwrap();
    let target_c = std::ffi::CString::new(target).unwrap();
    let mut blob = None;
    let mut err_blob = None;
    let res = unsafe {
        D3DCompile(
            src.as_ptr() as *const c_void,
            src.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_c.as_ptr() as *const u8),
            PCSTR(target_c.as_ptr() as *const u8),
            0,
            0,
            &mut blob,
            Some(&mut err_blob),
        )
    };
    if let Err(e) = res {
        let detail = err_blob
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer() as *const u8,
                    b.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        bail!("HLSL D3DCompile({target}) failed: {e}: {detail}");
    }
    let blob = blob.unwrap();
    let bytes = unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
            .to_vec()
    };
    Ok(bytes)
}

struct IntermediateBlitResources {
    _tmp_y: ID3D11Texture2D,
    _tmp_uv: ID3D11Texture2D,
    tmp_y_uav: ID3D11UnorderedAccessView,
    tmp_uv_uav: ID3D11UnorderedAccessView,
    tmp_y_srv: ID3D11ShaderResourceView,
    tmp_uv_srv: ID3D11ShaderResourceView,
    vs: ID3D11VertexShader,
    ps_y: ID3D11PixelShader,
    ps_uv: ID3D11PixelShader,
}

struct VideoProcessorResources {
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

pub struct D3d11Nv12Converter {
    pub d3d11: Arc<SharedD3d11Context>,
    pub mode: Nv12ConverterMode,
    pub color_matrix: ColorMatrix,
    pub color_range: ColorRange,
    pub width: u32,
    pub height: u32,
    pub aligned_width: u32,
    pub aligned_height: u32,
    cs: Option<ID3D11ComputeShader>,
    cb: Option<ID3D11Buffer>,
    intermediate: Option<IntermediateBlitResources>,
    vp: Option<VideoProcessorResources>,
}

unsafe impl Send for D3d11Nv12Converter {}

impl D3d11Nv12Converter {
    pub fn new(
        d3d11: Arc<SharedD3d11Context>,
        width: u32,
        height: u32,
        requested_mode: Nv12ConverterMode,
        color_matrix: ColorMatrix,
        color_range: ColorRange,
    ) -> Result<Self> {
        let mode = d3d11.resolve_converter_mode(requested_mode)?;
        let aligned_width = width.next_multiple_of(16);
        let aligned_height = height.next_multiple_of(16);

        let (cs, cb) = match mode {
            Nv12ConverterMode::ComputeDirect | Nv12ConverterMode::ComputeIntermediate => {
                let bytecode = compile_hlsl(HLSL_RGBA_TO_NV12_CS, "main", "cs_5_0")?;
                let mut shader: Option<ID3D11ComputeShader> = None;
                unsafe {
                    d3d11
                        .device
                        .CreateComputeShader(&bytecode, None, Some(&mut shader))
                        .context("CreateComputeShader(rgba_to_nv12)")?;
                }
                let params = color_params(
                    color_matrix,
                    color_range,
                    width,
                    height,
                    aligned_width,
                    aligned_height,
                );
                let cb_desc = D3D11_BUFFER_DESC {
                    ByteWidth: std::mem::size_of::<YuvColorParams>() as u32,
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                };
                let sub = D3D11_SUBRESOURCE_DATA {
                    pSysMem: &params as *const YuvColorParams as *const c_void,
                    ..Default::default()
                };
                let mut cb: Option<ID3D11Buffer> = None;
                unsafe {
                    d3d11
                        .device
                        .CreateBuffer(&cb_desc, Some(&sub), Some(&mut cb))
                        .context("CreateBuffer(YuvColorParams)")?;
                }
                (shader, cb)
            }
            _ => (None, None),
        };

        let intermediate = if mode == Nv12ConverterMode::ComputeIntermediate {
            let make_plane = |w: u32, h: u32, fmt: DXGI_FORMAT| -> Result<(
                ID3D11Texture2D,
                ID3D11UnorderedAccessView,
                ID3D11ShaderResourceView,
            )> {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: fmt,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_SHADER_RESOURCE.0)
                        as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut tex = None;
                unsafe { d3d11.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
                let tex = tex.unwrap();
                let mut uav = None;
                unsafe { d3d11.device.CreateUnorderedAccessView(&tex, None, Some(&mut uav))? };
                let mut srv = None;
                unsafe { d3d11.device.CreateShaderResourceView(&tex, None, Some(&mut srv))? };
                Ok((tex, uav.unwrap(), srv.unwrap()))
            };
            let (tmp_y, tmp_y_uav, tmp_y_srv) =
                make_plane(aligned_width, aligned_height, DXGI_FORMAT_R8_UNORM)?;
            let (tmp_uv, tmp_uv_uav, tmp_uv_srv) = make_plane(
                aligned_width / 2,
                aligned_height / 2,
                DXGI_FORMAT_R8G8_UNORM,
            )?;
            let vs_bc = compile_hlsl(HLSL_BLIT_VS, "main", "vs_5_0")?;
            let ps_y_bc = compile_hlsl(HLSL_BLIT_Y_PS, "main", "ps_5_0")?;
            let ps_uv_bc = compile_hlsl(HLSL_BLIT_UV_PS, "main", "ps_5_0")?;
            let mut vs = None;
            let mut ps_y = None;
            let mut ps_uv = None;
            unsafe {
                d3d11.device.CreateVertexShader(&vs_bc, None, Some(&mut vs))?;
                d3d11.device.CreatePixelShader(&ps_y_bc, None, Some(&mut ps_y))?;
                d3d11.device.CreatePixelShader(&ps_uv_bc, None, Some(&mut ps_uv))?;
            }
            Some(IntermediateBlitResources {
                _tmp_y: tmp_y,
                _tmp_uv: tmp_uv,
                tmp_y_uav,
                tmp_uv_uav,
                tmp_y_srv,
                tmp_uv_srv,
                vs: vs.unwrap(),
                ps_y: ps_y.unwrap(),
                ps_uv: ps_uv.unwrap(),
            })
        } else {
            None
        };

        let vp = if mode == Nv12ConverterMode::VideoProcessor {
            let vdev = d3d11
                .video_device
                .as_ref()
                .context("ID3D11VideoDevice unavailable")?;
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: 60,
                    Denominator: 1,
                },
                InputWidth: width,
                InputHeight: height,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: 60,
                    Denominator: 1,
                },
                OutputWidth: aligned_width,
                OutputHeight: aligned_height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerator = unsafe { vdev.CreateVideoProcessorEnumerator(&content)? };
            let processor = unsafe { vdev.CreateVideoProcessor(&enumerator, 0)? };
            let vctx = d3d11
                .video_context
                .as_ref()
                .context("ID3D11VideoContext unavailable")?;
            let src_rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            let dst_rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            unsafe {
                vctx.VideoProcessorSetStreamFrameFormat(
                    &processor,
                    0,
                    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                );
                vctx.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src_rect));
                vctx.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst_rect));
                vctx.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst_rect));
                vctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
                if let Some(vctx1) = &d3d11.video_context1 {
                    let in_cs = match color_matrix {
                        ColorMatrix::Bt2020 => DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P2020,
                        _ => DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                    };
                    let out_cs = match (color_matrix, color_range) {
                        (ColorMatrix::Bt601, ColorRange::Limited) => {
                            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601
                        }
                        (ColorMatrix::Bt601, ColorRange::Full) => {
                            DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601
                        }
                        (ColorMatrix::Bt709, ColorRange::Limited) => {
                            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
                        }
                        (ColorMatrix::Bt709, ColorRange::Full) => {
                            DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709
                        }
                        (ColorMatrix::Bt2020, ColorRange::Limited) => {
                            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020
                        }
                        (ColorMatrix::Bt2020, ColorRange::Full) => {
                            DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020
                        }
                    };
                    vctx1.VideoProcessorSetStreamColorSpace1(&processor, 0, in_cs);
                    vctx1.VideoProcessorSetOutputColorSpace1(&processor, out_cs);
                } else {
                    // Fallback to D3D11.0 VideoProcessor color-space API when ID3D11VideoContext1 is unavailable.
                    let in_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                        _bitfield: 0, // RGB Full Range
                    };
                    let mut out_bits: u32 = 0;
                    if color_matrix != ColorMatrix::Bt601 {
                        out_bits |= 1 << 2; // YCbCr_Matrix = 1 (BT.709)
                    }
                    if color_range == ColorRange::Limited {
                        out_bits |= 1 << 4; // Nominal_Range = 1 (16-235)
                    } else {
                        out_bits |= 2 << 4; // Nominal_Range = 2 (0-255)
                    }
                    let out_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                        _bitfield: out_bits,
                    };
                    vctx.VideoProcessorSetStreamColorSpace(&processor, 0, &in_cs);
                    vctx.VideoProcessorSetOutputColorSpace(&processor, &out_cs);
                }
            }
            Some(VideoProcessorResources {
                enumerator,
                processor,
            })
        } else {
            None
        };

        Ok(Self {
            d3d11,
            mode,
            color_matrix,
            color_range,
            width,
            height,
            aligned_width,
            aligned_height,
            cs,
            cb,
            intermediate,
            vp,
        })
    }

    pub fn vp_enumerator(&self) -> Option<&ID3D11VideoProcessorEnumerator> {
        self.vp.as_ref().map(|v| &v.enumerator)
    }

    /// Dispatches GPU RGBA -> NV12 conversion directly into `dst_slot`.
    /// Caller must hold `self.d3d11.ctx_lock` or ensure exclusive D3D11 context access.
    pub fn convert_locked(
        &self,
        src_srv: &ID3D11ShaderResourceView,
        src_vp_view: Option<&ID3D11VideoProcessorInputView>,
        dst_slot: &Nv12PoolSlot,
    ) -> Result<()> {
        let ctx = &self.d3d11.context;
        match self.mode {
            Nv12ConverterMode::ComputeDirect => {
                let cs = self.cs.as_ref().context("compute shader missing")?;
                let uav_y = dst_slot
                    .uav_y
                    .as_ref()
                    .context("NV12 slot missing Y UAV")?;
                let uav_uv = dst_slot
                    .uav_uv
                    .as_ref()
                    .context("NV12 slot missing UV UAV")?;
                let gx = (self.aligned_width / 2).div_ceil(16);
                let gy = (self.aligned_height / 2).div_ceil(16);
                unsafe {
                    ctx.CSSetShader(cs, None);
                    ctx.CSSetConstantBuffers(0, Some(&[self.cb.clone()]));
                    ctx.CSSetShaderResources(0, Some(&[Some(src_srv.clone())]));
                    let uavs = [Some(uav_y.clone()), Some(uav_uv.clone())];
                    ctx.CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
                    ctx.Dispatch(gx, gy, 1);
                    let null_uavs: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
                    let null_srvs: [Option<ID3D11ShaderResourceView>; 1] = [None];
                    ctx.CSSetUnorderedAccessViews(0, 2, Some(null_uavs.as_ptr()), None);
                    ctx.CSSetShaderResources(0, Some(&null_srvs));
                }
            }
            Nv12ConverterMode::ComputeIntermediate => {
                let cs = self.cs.as_ref().context("compute shader missing")?;
                let inter = self
                    .intermediate
                    .as_ref()
                    .context("intermediate blit resources missing")?;
                let rtv_y = dst_slot.rtv_y.as_ref().context("NV12 slot missing Y RTV")?;
                let rtv_uv = dst_slot
                    .rtv_uv
                    .as_ref()
                    .context("NV12 slot missing UV RTV")?;
                let gx = (self.aligned_width / 2).div_ceil(16);
                let gy = (self.aligned_height / 2).div_ceil(16);
                unsafe {
                    // Pass 1: Compute into intermediate R8 + RG8 UAVs
                    ctx.CSSetShader(cs, None);
                    ctx.CSSetConstantBuffers(0, Some(&[self.cb.clone()]));
                    ctx.CSSetShaderResources(0, Some(&[Some(src_srv.clone())]));
                    let uavs = [
                        Some(inter.tmp_y_uav.clone()),
                        Some(inter.tmp_uv_uav.clone()),
                    ];
                    ctx.CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
                    ctx.Dispatch(gx, gy, 1);
                    let null_uavs: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
                    let null_srvs: [Option<ID3D11ShaderResourceView>; 1] = [None];
                    ctx.CSSetUnorderedAccessViews(0, 2, Some(null_uavs.as_ptr()), None);
                    ctx.CSSetShaderResources(0, Some(&null_srvs));

                    // Pass 2: Fullscreen triangle RTV blit into NV12 Y & UV planes.
                    // Explicitly reset rasterizer/blend/depth state since Intel MFX shares the D3D11 immediate context.
                    ctx.RSSetState(None);
                    ctx.OMSetBlendState(None, None, 0xFFFF_FFFF);
                    ctx.OMSetDepthStencilState(None, 0);
                    ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                    ctx.IASetInputLayout(None);
                    ctx.VSSetShader(&inter.vs, None);

                    let vp_y = D3D11_VIEWPORT {
                        TopLeftX: 0.0,
                        TopLeftY: 0.0,
                        Width: self.aligned_width as f32,
                        Height: self.aligned_height as f32,
                        MinDepth: 0.0,
                        MaxDepth: 1.0,
                    };
                    ctx.RSSetViewports(Some(&[vp_y]));
                    ctx.OMSetRenderTargets(Some(&[Some(rtv_y.clone())]), None);
                    ctx.PSSetShader(&inter.ps_y, None);
                    ctx.PSSetShaderResources(0, Some(&[Some(inter.tmp_y_srv.clone())]));
                    ctx.Draw(3, 0);

                    let vp_uv = D3D11_VIEWPORT {
                        TopLeftX: 0.0,
                        TopLeftY: 0.0,
                        Width: (self.aligned_width / 2) as f32,
                        Height: (self.aligned_height / 2) as f32,
                        MinDepth: 0.0,
                        MaxDepth: 1.0,
                    };
                    ctx.RSSetViewports(Some(&[vp_uv]));
                    ctx.OMSetRenderTargets(Some(&[Some(rtv_uv.clone())]), None);
                    ctx.PSSetShader(&inter.ps_uv, None);
                    ctx.PSSetShaderResources(0, Some(&[Some(inter.tmp_uv_srv.clone())]));
                    ctx.Draw(3, 0);

                    ctx.OMSetRenderTargets(None, None);
                    ctx.PSSetShaderResources(0, Some(&[None]));
                }
            }
            Nv12ConverterMode::VideoProcessor => {
                let vp_res = self.vp.as_ref().context("VideoProcessor missing")?;
                let vctx = self
                    .d3d11
                    .video_context
                    .as_ref()
                    .context("ID3D11VideoContext missing")?;
                let in_view = src_vp_view.context("VideoProcessor input view missing")?;
                let out_view = dst_slot
                    .vp_out_view
                    .as_ref()
                    .context("NV12 slot missing VideoProcessor output view")?;
                let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
                    Enable: true.into(),
                    pInputSurface: std::mem::ManuallyDrop::new(Some(in_view.clone())),
                    ..Default::default()
                }];
                let blt_res =
                    unsafe { vctx.VideoProcessorBlt(&vp_res.processor, out_view, 0, &streams) };
                unsafe {
                    std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
                }
                blt_res.context("ID3D11VideoContext::VideoProcessorBlt")?;
            }
            Nv12ConverterMode::Auto => unreachable!("resolved in new()"),
        }
        Ok(())
    }
}

// ============================================================================
// D3D12 (Skia) <-> D3D11 Zero-Copy Shared RGBA Ring
// ============================================================================

struct SharedRgbaSlot {
    _d3d12_resource: ID3D12Resource,
    surface: Surface,
    _d3d11_texture: ID3D11Texture2D,
    d3d11_srv: ID3D11ShaderResourceView,
    d3d11_vp_in: Option<ID3D11VideoProcessorInputView>,
    last_d3d11_done_seq: u64,
}

pub struct D3d12SharedRgbaRing {
    pub width: u32,
    pub height: u32,
    pub d3d11: Arc<SharedD3d11Context>,
    pub pool: Arc<Nv12SurfacePool>,
    pub converter: D3d11Nv12Converter,
    d3d12_queue: ID3D12CommandQueue,
    d3d12_to_d3d11_fence12: ID3D12Fence,
    d3d12_to_d3d11_fence11: ID3D11Fence,
    d3d11_to_d3d12_fence12: ID3D12Fence,
    d3d11_to_d3d12_fence11: ID3D11Fence,
    cpu_wait_event: HANDLE,
    fence_seq: u64,
    last_d3d11_signaled_seq: u64,
    slots: Vec<SharedRgbaSlot>,
    next_slot: usize,
}

impl D3d12SharedRgbaRing {
    pub fn new(
        d3d12_device: &ID3D12Device,
        d3d12_queue: ID3D12CommandQueue,
        skia_context: &mut DirectContext,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let luid = unsafe { d3d12_device.GetAdapterLuid() };
        let d3d11 = SharedD3d11Context::get_or_create_for_luid(luid)?;
        let requested_mode = Nv12ConverterMode::from_env();
        let color_matrix = ColorMatrix::from_env();
        let color_range = ColorRange::from_env();

        let converter = D3d11Nv12Converter::new(
            Arc::clone(&d3d11),
            width,
            height,
            requested_mode,
            color_matrix,
            color_range,
        )?;
        let async_depth = std::env::var("GOOSY_QSV_ASYNC_DEPTH")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| (1..=16).contains(&v))
            .unwrap_or(4);
        let gop_ref_dist = std::env::var("GOOSY_QSV_GOP_REF_DIST")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| (1..=8).contains(&v))
            .unwrap_or(1);
        let queue_depth = async_depth.clamp(2, 8);
        let min_safe_pool = (async_depth + gop_ref_dist + queue_depth + 2).max(6);
        let pool_size = std::env::var("GOOSY_QSV_POOL_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| (4..=64).contains(&v))
            .unwrap_or(DEFAULT_NV12_POOL_SIZE)
            .max(min_safe_pool);
        let pool = Nv12SurfacePool::new_for_mode(
            Arc::clone(&d3d11),
            width,
            height,
            pool_size,
            converter.mode,
            converter.vp_enumerator(),
        )?;

        let (d3d12_to_d3d11_fence12, d3d12_to_d3d11_fence11) =
            create_shared_fence_pair(d3d12_device, &d3d11.device5)?;
        let (d3d11_to_d3d12_fence12, d3d11_to_d3d12_fence11) =
            create_shared_fence_pair(d3d12_device, &d3d11.device5)?;

        let mut slots = Vec::with_capacity(RGBA_RING_SIZE);
        for idx in 0..RGBA_RING_SIZE {
            let heap_props = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let res_desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: width as u64,
                Height: height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                    | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
                ..Default::default()
            };
            let mut d3d12_res: Option<ID3D12Resource> = None;
            unsafe {
                d3d12_device
                    .CreateCommittedResource(
                        &heap_props,
                        D3D12_HEAP_FLAG_SHARED,
                        &res_desc,
                        D3D12_RESOURCE_STATE_COMMON,
                        None,
                        &mut d3d12_res,
                    )
                    .with_context(|| format!("allocate shared D3D12 RGBA slot #{idx}"))?;
            }
            let d3d12_res = d3d12_res.unwrap();

            // Open on D3D11 device (zero-copy same GPU allocation!)
            let nt_handle = unsafe {
                d3d12_device
                    .CreateSharedHandle(&d3d12_res, None, GENERIC_ALL.0, None)
                    .context("CreateSharedHandle(D3D12 RGBA texture)")?
            };
            let open_res: Result<ID3D11Texture2D> = unsafe {
                d3d11
                    .device1
                    .OpenSharedResource1(nt_handle)
                    .context("ID3D11Device1::OpenSharedResource1(RGBA)")
            };
            unsafe {
                let _ = CloseHandle(nt_handle);
            }
            let d3d11_texture = open_res?;

            let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                    },
                },
            };
            let mut d3d11_srv = None;
            unsafe {
                d3d11
                    .device
                    .CreateShaderResourceView(&d3d11_texture, Some(&srv_desc), Some(&mut d3d11_srv))
                    .context("create SRV for shared RGBA texture")?;
            }
            let d3d11_srv = d3d11_srv.unwrap();

            let d3d11_vp_in = if let (Some(vdev), Some(vpenum)) =
                (&d3d11.video_device, converter.vp_enumerator())
            {
                let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV {
                            MipSlice: 0,
                            ArraySlice: 0,
                        },
                    },
                };
                let mut view = None;
                unsafe {
                    vdev.CreateVideoProcessorInputView(
                        &d3d11_texture,
                        vpenum,
                        &in_desc,
                        Some(&mut view),
                    )
                    .context("create VP input view for shared RGBA texture")?;
                }
                view
            } else {
                None
            };

            // Wrap shared D3D12 resource in Skia BackendRenderTarget
            let tex_info = skia_d3d::TextureResourceInfo {
                resource: d3d12_res.clone(),
                alloc: None,
                resource_state: D3D12_RESOURCE_STATE_COMMON,
                format: DXGI_FORMAT_R8G8B8A8_UNORM,
                sample_count: 1,
                level_count: 1,
                sample_quality_pattern: DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN,
                protected: gpu::Protected::No,
            };
            let backend_rt =
                backend_render_targets::make_d3d((width as i32, height as i32), &tex_info);
            let surface = gpu::surfaces::wrap_backend_render_target(
                skia_context,
                &backend_rt,
                SurfaceOrigin::TopLeft,
                ColorType::RGBA8888,
                ColorSpace::new_srgb(),
                None,
            )
            .context("wrap shared D3D12 RGBA texture as Skia surface")?;

            slots.push(SharedRgbaSlot {
                _d3d12_resource: d3d12_res,
                surface,
                _d3d11_texture: d3d11_texture,
                d3d11_srv,
                d3d11_vp_in,
                last_d3d11_done_seq: 0,
            });
        }

        let cpu_wait_event = unsafe { CreateEventW(None, false, false, None)? };

        Ok(Self {
            width,
            height,
            d3d11,
            pool,
            converter,
            d3d12_queue,
            d3d12_to_d3d11_fence12,
            d3d12_to_d3d11_fence11,
            d3d11_to_d3d12_fence12,
            d3d11_to_d3d12_fence11,
            cpu_wait_event,
            fence_seq: 0,
            last_d3d11_signaled_seq: 0,
            slots,
            next_slot: 0,
        })
    }

    /// Renders a frame via Skia on D3D12 into the next shared RGBA ring slot,
    /// performs GPU-only fence handoff to D3D11, converts RGBA -> NV12 directly
    /// into a pooled NV12 D3D11 surface, and returns `WinGpuFrame` with 0 CPU pixel copies.
    pub fn render_and_convert<F>(
        &mut self,
        skia_context: &mut DirectContext,
        info: &skia_safe::ImageInfo,
        row_bytes: usize,
        read_pixels_dst: Option<&mut Vec<u8>>,
        draw: F,
    ) -> Result<WinGpuFrame>
    where
        F: FnOnce(&skia_safe::Canvas) -> Result<()>,
    {
        let slot_idx = self.next_slot;
        self.next_slot = (self.next_slot + 1) % self.slots.len();

        // Ensure D3D12 does not overwrite slot[slot_idx] before D3D11 finishes reading it.
        let prev_done = self.slots[slot_idx].last_d3d11_done_seq;
        if prev_done > 0 {
            unsafe {
                self.d3d12_queue
                    .Wait(&self.d3d11_to_d3d12_fence12, prev_done)
                    .context("D3D12 queue GPU wait on D3D11 done fence")?;
            }
        }

        // 1. Draw on Skia D3D12 canvas
        draw(self.slots[slot_idx].surface.canvas())?;

        // Optional CPU readback ONLY when requested (e.g. GUI live preview every N frames)
        if let Some(pixels) = read_pixels_dst {
            pixels.resize(info.compute_byte_size(row_bytes), 0);
            if !self.slots[slot_idx].surface.read_pixels(
                info,
                pixels,
                row_bytes,
                skia_safe::IPoint::new(0, 0),
            ) {
                bail!("read RGBA preview pixels from shared D3D12 surface");
            }
        }

        // Transition wrapped render target to COMMON/PRESENT and submit asynchronously (NO CPU sync!)
        skia_context.flush_surface_with_access(
            &mut self.slots[slot_idx].surface,
            BackendSurfaceAccess::Present,
            &FlushInfo::default(),
        );
        skia_context.submit(None);
        if skia_context.is_device_lost() || skia_context.oomed() {
            bail!("Skia D3D12 submission failed (device lost or OOM)");
        }

        // 2. Signal D3D12 -> D3D11 shared fence
        self.fence_seq += 1;
        let seq = self.fence_seq;
        unsafe {
            self.d3d12_queue
                .Signal(&self.d3d12_to_d3d11_fence12, seq)
                .context("signal D3D12 -> D3D11 shared fence")?;
        }

        // 3. Acquire free pooled NV12 D3D11 surface (backpressured if encoder queue is full)
        let lease = self.pool.acquire();

        // 4. GPU-wait on D3D12 fence, run RGBA -> NV12 directly into NV12 slot, signal done fence
        {
            let _guard = self.d3d11.lock_immediate_context();
            unsafe {
                self.d3d11
                    .context4
                    .Wait(&self.d3d12_to_d3d11_fence11, seq)
                    .context("D3D11 GPU wait on D3D12 ready fence")?;
            }
            self.converter.convert_locked(
                &self.slots[slot_idx].d3d11_srv,
                self.slots[slot_idx].d3d11_vp_in.as_ref(),
                lease.slot(),
            )?;
            unsafe {
                self.d3d11
                    .context4
                    .Signal(&self.d3d11_to_d3d12_fence11, seq)
                    .context("signal D3D11 -> D3D12 done fence")?;
                // Flush so WDDM schedules the Compute/VP work before QSV Video Engine reads NV12.
                self.d3d11.context.Flush();
            }
        }
        self.last_d3d11_signaled_seq = seq;
        self.slots[slot_idx].last_d3d11_done_seq = seq;

        Ok(WinGpuFrame {
            lease,
            width: self.width,
            height: self.height,
            aligned_width: self.pool.aligned_width,
            aligned_height: self.pool.aligned_height,
            color_matrix: self.converter.color_matrix,
            color_range: self.converter.color_range,
            converter_used: self.converter.mode,
        })
    }

    /// Waits for all signaled D3D11 conversion work to finish (used on Drop / teardown).
    pub fn drain_gpu(&self) {
        if self.last_d3d11_signaled_seq > 0 {
            unsafe {
                if self
                    .d3d11_to_d3d12_fence12
                    .SetEventOnCompletion(self.last_d3d11_signaled_seq, self.cpu_wait_event)
                    .is_ok()
                {
                    WaitForSingleObject(self.cpu_wait_event, GPU_DRAIN_TIMEOUT_MS);
                }
            }
        }
    }
}

impl Drop for D3d12SharedRgbaRing {
    fn drop(&mut self) {
        self.drain_gpu();
        self.slots.clear();
        unsafe {
            let _ = CloseHandle(self.cpu_wait_event);
        }
    }
}

fn create_shared_fence_pair(
    d3d12_device: &ID3D12Device,
    d3d11_device5: &ID3D11Device5,
) -> Result<(ID3D12Fence, ID3D11Fence)> {
    let fence12: ID3D12Fence = unsafe {
        d3d12_device
            .CreateFence(0, D3D12_FENCE_FLAG_SHARED)
            .context("CreateFence(D3D12_FENCE_FLAG_SHARED)")?
    };
    let handle = unsafe {
        d3d12_device
            .CreateSharedHandle(&fence12, None, GENERIC_ALL.0, None)
            .context("CreateSharedHandle(ID3D12Fence)")?
    };
    let mut fence11: Option<ID3D11Fence> = None;
    let res = unsafe {
        d3d11_device5
            .OpenSharedFence(handle, &mut fence11)
            .context("ID3D11Device5::OpenSharedFence")
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    res?;
    Ok((fence12, fence11.unwrap()))
}
