//! Direct Intel QSV (oneVPL / Media SDK) zero-copy hardware encoder over D3D11
//! with asynchronous encode pipeline, surface lifetime tracking, and FFmpeg MP4 muxing.

use super::d3d11_pipeline::{
    D3d11Nv12Converter, DEFAULT_NV12_POOL_SIZE, Nv12SurfaceLease, Nv12SurfacePool,
    SharedD3d11Context,
};
use super::mfx_ffi::*;
use super::{
    ColorMatrix, ColorRange, Nv12ConverterMode, WinGpuFrame, record_cpu_fallback_frame,
    record_gpu_frame_encoded,
};
use crate::encoder::VideoCodec;
use crate::frame::RenderedFrame;
use anyhow::{Context, Result, bail};
use ffmpeg_next as ffmpeg;
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex, mpsc};
use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2D;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QsvLowPowerMode {
    /// Auto: uses VME (`OFF`) on Gen 9.5 / legacy MSDK 1.x where VME is faster and
    /// supports both H.264 & HEVC, or tries `ON` -> `OFF` based on hardware query.
    #[default]
    Auto,
    /// Force VDENC low-power fixed-function pipeline (`MFX_CODINGOPTION_ON`).
    On,
    /// Force VME + EU pipeline (`MFX_CODINGOPTION_OFF`).
    Off,
}

impl QsvLowPowerMode {
    pub fn from_env() -> Self {
        match std::env::var("GOOSY_QSV_LOW_POWER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "1" | "on" | "true" | "vdenc" => Self::On,
            "0" | "off" | "false" | "vme" => Self::Off,
            _ => Self::Auto,
        }
    }
}

#[derive(Clone, Debug)]
pub struct QsvEncodeConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub target_kbps: u32,
    pub max_kbps: u32,
    pub gop_size: u16,
    pub gop_ref_dist: u16,
    pub async_depth: u16,
    pub target_usage: u16,
    pub low_power: QsvLowPowerMode,
    pub color_matrix: ColorMatrix,
    pub color_range: ColorRange,
}

impl QsvEncodeConfig {
    pub fn from_env_defaults(codec: VideoCodec, width: u32, height: u32, fps: u32) -> Self {
        let gop_size = std::env::var("GOOSY_QSV_GOP")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|&v| v > 0)
            .unwrap_or_else(|| (fps.min(u16::MAX as u32) as u16).saturating_mul(2).max(30));
        let gop_ref_dist = std::env::var("GOOSY_QSV_GOP_REF_DIST")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|&v| v >= 1 && v <= 8)
            .unwrap_or(1); // 1 = IPPP low-latency, no B-frame reorder delay
        let async_depth = std::env::var("GOOSY_QSV_ASYNC_DEPTH")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|&v| v >= 1 && v <= 16)
            .unwrap_or(4);
        let target_usage = std::env::var("GOOSY_QSV_TU")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|&v| v >= 1 && v <= 7)
            .unwrap_or(4); // 4 = balanced (1=best quality .. 7=fastest)
        let target_kbps = std::env::var("GOOSY_QSV_BITRATE_KBPS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&v| v >= 500)
            .unwrap_or(8_000);
        Self {
            codec,
            width,
            height,
            fps,
            target_kbps,
            max_kbps: target_kbps.saturating_mul(3) / 2,
            gop_size,
            gop_ref_dist,
            async_depth,
            target_usage,
            low_power: QsvLowPowerMode::from_env(),
            color_matrix: ColorMatrix::from_env(),
            color_range: ColorRange::from_env(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct QsvCapabilities {
    pub runtime_dll: &'static str,
    pub mfx_version: MfxVersion,
    pub adapter_name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub supports_direct_nv12_uav: bool,
    pub supports_vp_rgba_to_nv12: bool,
    pub supports_vme: bool,
    pub supports_vdenc: bool,
}

/// Probes whether Intel QSV + D3D11 hardware encoding is available for the requested
/// codec, resolution, and frame rate without altering global encoder state.
pub fn probe_qsv_capabilities(
    codec: VideoCodec,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<QsvCapabilities> {
    validate_qsv_dimensions(width, height)?;
    let mfx = MfxRuntime::global()?;
    let d3d11 = SharedD3d11Context::get_or_create_intel()?;
    d3d11.check_device_health()?;

    let session_guard = mfx.create_d3d11_session()?;
    let session = session_guard.session;
    let mut ver = MfxVersion::default();
    unsafe {
        let _ = (mfx.query_version)(session, &mut ver);
    }
    let st = unsafe { (mfx.set_handle)(session, MFX_HANDLE_D3D11_DEVICE, d3d11.device.as_raw()) };
    if st < 0 {
        bail!(
            "MFXVideoCORE_SetHandle(D3D11_DEVICE) failed: {st} ({})",
            mfx_status_name(st)
        );
    }

    let check_lp = |lp: u16| -> bool {
        let mut cfg = QsvEncodeConfig::from_env_defaults(codec, width, height, fps);
        cfg.low_power = if lp == MFX_CODINGOPTION_ON {
            QsvLowPowerMode::On
        } else {
            QsvLowPowerMode::Off
        };
        let mut in_p = build_video_param(&cfg, lp, None);
        let mut out_p = in_p;
        let st = unsafe { (mfx.enc_query)(session, &mut in_p, &mut out_p) };
        st >= 0
    };
    let supports_vme = check_lp(MFX_CODINGOPTION_OFF);
    let supports_vdenc = check_lp(MFX_CODINGOPTION_ON);
    drop(session_guard);

    if !supports_vme && !supports_vdenc {
        bail!(
            "Intel QSV on {} (MFX v{}.{}) does not support {:?} {}x{}@{}fps",
            d3d11.adapter_name,
            ver.major,
            ver.minor,
            codec,
            width,
            height,
            fps
        );
    }

    Ok(QsvCapabilities {
        runtime_dll: mfx.dll_name,
        mfx_version: ver,
        adapter_name: d3d11.adapter_name.clone(),
        vendor_id: d3d11.vendor_id,
        device_id: d3d11.device_id,
        supports_direct_nv12_uav: d3d11.supports_direct_nv12_uav,
        supports_vp_rgba_to_nv12: d3d11.supports_vp_rgba_to_nv12,
        supports_vme,
        supports_vdenc,
    })
}

fn validate_qsv_dimensions(width: u32, height: u32) -> Result<()> {
    if width < 64
        || height < 64
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || width > (u16::MAX as u32 - 16)
        || height > (u16::MAX as u32 - 16)
    {
        bail!("QSV hardware path requires even dimensions in 64..=65518, got {width}x{height}");
    }
    Ok(())
}

// ============================================================================
// External mfxFrameAllocator for Internal Reconstructed Surfaces & Zero-Copy GetHDL
// ============================================================================

#[allow(dead_code)]
enum AllocBacking {
    Texture(ID3D11Texture2D),
    Buffer(ID3D11Buffer),
}

struct InternalAllocBlock {
    _resources: Vec<AllocBacking>,
    hdl_pairs: Vec<MfxHdlPair>,
    mids: Vec<MfxMemId>,
}

struct QsvAllocatorState {
    device: ID3D11Device,
    blocks: Mutex<Vec<Box<InternalAllocBlock>>>,
}

unsafe extern "C" fn qsv_alloc_cb(
    pthis: *mut c_void,
    request: *mut MfxFrameAllocRequest,
    response: *mut MfxFrameAllocResponse,
) -> MfxStatus {
    if pthis.is_null() || request.is_null() || response.is_null() {
        return MFX_ERR_UNKNOWN;
    }
    let state = unsafe { &*(pthis as *const QsvAllocatorState) };
    let req = unsafe { &*request };
    let count = req.num_frame_suggested.max(req.num_frame_min).max(1) as usize;

    let mut resources = Vec::with_capacity(count);
    let mut hdl_pairs = Vec::with_capacity(count);

    if req.info.fourcc == MFX_FOURCC_P8 {
        // Internal scratch / PAK / BRC buffers requested by newer QSV generations.
        let byte_width = (req.info.width as u32)
            .saturating_mul((req.info.height as u32).max(1))
            .max(1);
        for _ in 0..count {
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: byte_width,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: 0,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut buf: Option<ID3D11Buffer> = None;
            if unsafe { state.device.CreateBuffer(&desc, None, Some(&mut buf)) }.is_err() {
                return MFX_ERR_MEMORY_ALLOC;
            }
            let Some(buf) = buf else {
                return MFX_ERR_MEMORY_ALLOC;
            };
            hdl_pairs.push(MfxHdlPair {
                first: buf.as_raw(),
                second: null_mut(),
            });
            resources.push(AllocBacking::Buffer(buf));
        }
    } else {
        let format = match req.info.fourcc {
            MFX_FOURCC_NV12 => DXGI_FORMAT_NV12,
            MFX_FOURCC_P010 => DXGI_FORMAT_P010,
            _ => return MFX_ERR_UNSUPPORTED,
        };
        // Individual ArraySize=1 textures avoid UHD 630's E_INVALIDARG on NV12 texture arrays.
        let bind_candidates: &[u32] = &[
            D3D11_BIND_DECODER.0 as u32,
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        ];

        for _ in 0..count {
            let mut created: Option<ID3D11Texture2D> = None;
            for &flags in bind_candidates {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: req.info.width as u32,
                    Height: req.info.height as u32,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: format,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: flags,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut tex: Option<ID3D11Texture2D> = None;
                if unsafe { state.device.CreateTexture2D(&desc, None, Some(&mut tex)) }.is_ok() {
                    created = tex;
                    break;
                }
            }
            let Some(tex) = created else {
                return MFX_ERR_MEMORY_ALLOC;
            };
            hdl_pairs.push(MfxHdlPair {
                first: tex.as_raw(),
                second: null_mut(),
            });
            resources.push(AllocBacking::Texture(tex));
        }
    }

    let mut block = Box::new(InternalAllocBlock {
        _resources: resources,
        hdl_pairs,
        mids: Vec::with_capacity(count),
    });
    for i in 0..count {
        let p = &mut block.hdl_pairs[i] as *mut MfxHdlPair as MfxMemId;
        block.mids.push(p);
    }
    let resp = unsafe { &mut *response };
    resp.mids = block.mids.as_mut_ptr();
    resp.num_frame_actual = count as u16;
    state
        .blocks
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(block);
    MFX_ERR_NONE
}

unsafe extern "C" fn qsv_lock_cb(
    _pthis: *mut c_void,
    _mid: MfxMemId,
    _ptr: *mut MfxFrameData,
) -> MfxStatus {
    MFX_ERR_UNSUPPORTED
}

unsafe extern "C" fn qsv_unlock_cb(
    _pthis: *mut c_void,
    _mid: MfxMemId,
    _ptr: *mut MfxFrameData,
) -> MfxStatus {
    MFX_ERR_NONE
}

unsafe extern "C" fn qsv_get_hdl_cb(
    _pthis: *mut c_void,
    mid: MfxMemId,
    handle: *mut MfxHdl,
) -> MfxStatus {
    if mid.is_null() || handle.is_null() {
        return MFX_ERR_UNKNOWN;
    }
    unsafe {
        let pair = &*(mid as *const MfxHdlPair);
        let out = &mut *(handle as *mut MfxHdlPair);
        *out = *pair;
    }
    MFX_ERR_NONE
}

unsafe extern "C" fn qsv_free_cb(
    pthis: *mut c_void,
    response: *mut MfxFrameAllocResponse,
) -> MfxStatus {
    if pthis.is_null() || response.is_null() {
        return MFX_ERR_NONE;
    }
    let state = unsafe { &*(pthis as *const QsvAllocatorState) };
    let resp = unsafe { &*response };
    if !resp.mids.is_null() {
        state
            .blocks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|b| b.mids.as_ptr() != resp.mids);
    }
    MFX_ERR_NONE
}

fn build_video_param(
    cfg: &QsvEncodeConfig,
    low_power: u16,
    ext_params: Option<(*mut *mut MfxExtBuffer, u16)>,
) -> MfxVideoParam {
    let aligned_w = cfg.width.next_multiple_of(16) as u16;
    let aligned_h = cfg.height.next_multiple_of(16) as u16;
    let mut param: MfxVideoParam = unsafe { std::mem::zeroed() };
    param.async_depth = cfg.async_depth;
    param.io_pattern = MFX_IOPATTERN_IN_VIDEO_MEMORY;
    param.mfx.codec_id = match cfg.codec {
        VideoCodec::H264 => MFX_CODEC_AVC,
        VideoCodec::Hevc => MFX_CODEC_HEVC,
    };
    param.mfx.target_usage = cfg.target_usage;
    param.mfx.low_power = low_power;
    param.mfx.rate_control_method = MFX_RATECONTROL_VBR;
    // Scale bitrates with BRCParamMultiplier if > 65535 kbps
    let mult = ((cfg.max_kbps / 60_000) + 1).min(u16::MAX as u32) as u16;
    param.mfx.brc_param_multiplier = mult;
    param.mfx.target_kbps = (cfg.target_kbps / mult as u32).clamp(1, u16::MAX as u32) as u16;
    param.mfx.max_kbps = (cfg.max_kbps / mult as u32)
        .clamp(param.mfx.target_kbps as u32, u16::MAX as u32) as u16;
    param.mfx.gop_pic_size = cfg.gop_size;
    param.mfx.gop_ref_dist = cfg.gop_ref_dist;
    param.mfx.gop_opt_flag = MFX_GOP_CLOSED;
    // In Intel oneVPL/MSDK: H.264 uses IdrInterval=0 for every I-frame as IDR,
    // whereas HEVC requires IdrInterval=1 (0 means only the first I-frame is IDR).
    param.mfx.idr_interval = match cfg.codec {
        VideoCodec::H264 => 0,
        VideoCodec::Hevc => 1,
    };
    param.mfx.num_slice = 1;
    param.mfx.frame_info.fourcc = MFX_FOURCC_NV12;
    param.mfx.frame_info.chroma_format = MFX_CHROMAFORMAT_YUV420;
    param.mfx.frame_info.pic_struct = MFX_PICSTRUCT_PROGRESSIVE;
    param.mfx.frame_info.width = aligned_w;
    param.mfx.frame_info.height = aligned_h;
    param.mfx.frame_info.crop_w = cfg.width as u16;
    param.mfx.frame_info.crop_h = cfg.height as u16;
    param.mfx.frame_info.frame_rate_ext_n = cfg.fps;
    param.mfx.frame_info.frame_rate_ext_d = 1;
    if let Some((ptr, count)) = ext_params {
        param.ext_param = ptr;
        param.num_ext_param = count;
    }
    param
}

// ============================================================================
// Direct QSV Session + Async Bitstream Ring + FFmpeg MP4 Muxer
// ============================================================================

struct InFlightSurface {
    // Stored inside UnsafeCell so Rust never creates `&MfxFrameSurface1` / `&mut MfxFrameSurface1`
    // aliasing references while Intel oneVPL/MFX holds the raw surface pointer.
    surface: UnsafeCell<MfxFrameSurface1>,
    ts_90k: u64,
    /// Set to `true` only after `MFXVideoCORE_SyncOperation` has completed and output
    /// this frame (establishing the driver-guaranteed happens-before boundary).
    output_synced: bool,
    _lease: Nv12SurfaceLease,
}

impl InFlightSurface {
    fn surface_ptr(&self) -> *mut MfxFrameSurface1 {
        self.surface.get()
    }
}

struct PendingBitstream {
    slot: usize,
    sync_point: MfxSyncPoint,
    submitted_ts_90k: u64,
}

struct CpuUploadFallback {
    rgba_tex: ID3D11Texture2D,
    rgba_srv: ID3D11ShaderResourceView,
    rgba_vp_in: Option<ID3D11VideoProcessorInputView>,
    converter: D3d11Nv12Converter,
    pool: Arc<Nv12SurfacePool>,
}

impl CpuUploadFallback {
    fn new(
        d3d11: Arc<SharedD3d11Context>,
        width: u32,
        height: u32,
        pool_size: usize,
        color_matrix: ColorMatrix,
        color_range: ColorRange,
    ) -> Result<Self> {
        let converter = D3d11Nv12Converter::new(
            Arc::clone(&d3d11),
            width,
            height,
            Nv12ConverterMode::from_env(),
            color_matrix,
            color_range,
        )?;
        let pool = Nv12SurfacePool::new_for_mode(
            Arc::clone(&d3d11),
            width,
            height,
            pool_size,
            converter.mode,
            converter.vp_enumerator(),
        )?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut rgba_tex = None;
        unsafe { d3d11.device.CreateTexture2D(&desc, None, Some(&mut rgba_tex))? };
        let rgba_tex = rgba_tex.unwrap();
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
        let mut rgba_srv = None;
        unsafe {
            d3d11
                .device
                .CreateShaderResourceView(&rgba_tex, Some(&srv_desc), Some(&mut rgba_srv))?;
        }
        let rgba_srv = rgba_srv.unwrap();
        let rgba_vp_in = if let (Some(vdev), Some(vpenum)) =
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
            let mut v = None;
            unsafe {
                vdev.CreateVideoProcessorInputView(&rgba_tex, vpenum, &in_desc, Some(&mut v))?;
            }
            v
        } else {
            None
        };
        Ok(Self {
            rgba_tex,
            rgba_srv,
            rgba_vp_in,
            converter,
            pool,
        })
    }

    fn upload_and_convert(&self, pixels: &[u8], row_bytes: usize) -> Result<Nv12SurfaceLease> {
        let lease = self.pool.acquire();
        let _guard = self.pool.d3d11.lock_immediate_context();
        unsafe {
            self.pool.d3d11.context.UpdateSubresource(
                &self.rgba_tex,
                0,
                None,
                pixels.as_ptr() as *const c_void,
                row_bytes as u32,
                0,
            );
        }
        self.converter
            .convert_locked(&self.rgba_srv, self.rgba_vp_in.as_ref(), lease.slot())?;
        unsafe {
            self.pool.d3d11.context.Flush();
        }
        Ok(lease)
    }
}

struct QsvSessionEncoder {
    mfx: &'static MfxRuntime,
    // Drop order: `session` closes MFX (which may invoke allocator callbacks) before
    // `in_flight_surfaces`, `bs_buffers`, `_allocator`, `_alloc_state`, and `d3d11`.
    session: OwnedMfxSession,
    in_flight_surfaces: Vec<Box<InFlightSurface>>,
    bs_buffers: Vec<Vec<u8>>,
    bitstreams: Vec<MfxBitstream>,
    free_bs_slots: VecDeque<usize>,
    pending: VecDeque<PendingBitstream>,
    _allocator: Box<MfxFrameAllocator>,
    _alloc_state: Box<QsvAllocatorState>,
    d3d11: Arc<SharedD3d11Context>,
    frame_info: MfxFrameInfo,
    config: QsvEncodeConfig,
    low_power_used: bool,
    output: ffmpeg::format::context::Output,
    stream_index: usize,
    stream_time_base: ffmpeg::Rational,
    packets_written: u64,
    cpu_upload: Option<CpuUploadFallback>,
}

impl QsvSessionEncoder {
    fn new(video_path: &Path, config: QsvEncodeConfig) -> Result<Self> {
        validate_qsv_dimensions(config.width, config.height)?;
        if config.fps == 0 || config.fps > MFX_TIMEBASE as u32 {
            bail!("QSV fps must be in 1..={}, got {}", MFX_TIMEBASE, config.fps);
        }
        ffmpeg::init().context("initialize ffmpeg-next for QSV MP4 muxer")?;
        let mfx = MfxRuntime::global()?;
        let d3d11 = SharedD3d11Context::get_or_create_intel()?;
        d3d11.check_device_health()?;

        // Declare allocator state BEFORE `session_guard` so reverse local drop order on
        // early `?` errors always closes the MFX session while allocator callbacks are live.
        let mut alloc_state = Box::new(QsvAllocatorState {
            device: d3d11.device.clone(),
            blocks: Mutex::new(Vec::new()),
        });
        let mut allocator = Box::new(MfxFrameAllocator {
            reserved: [0; 4],
            pthis: &mut *alloc_state as *mut QsvAllocatorState as *mut c_void,
            alloc: Some(qsv_alloc_cb),
            lock: Some(qsv_lock_cb),
            unlock: Some(qsv_unlock_cb),
            get_hdl: Some(qsv_get_hdl_cb),
            free: Some(qsv_free_cb),
        });

        let mut session_guard = mfx.create_d3d11_session()?;
        let session = session_guard.session;

        let mut ver = MfxVersion::default();
        unsafe {
            let _ = (mfx.query_version)(session, &mut ver);
        }

        let st =
            unsafe { (mfx.set_handle)(session, MFX_HANDLE_D3D11_DEVICE, d3d11.device.as_raw()) };
        if st < 0 {
            bail!(
                "MFXVideoCORE_SetHandle(D3D11_DEVICE) failed: {st} ({})",
                mfx_status_name(st)
            );
        }

        let st = unsafe { (mfx.set_allocator)(session, &mut *allocator) };
        if st < 0 {
            bail!(
                "MFXVideoCORE_SetFrameAllocator failed: {st} ({})",
                mfx_status_name(st)
            );
        }

        // Attach VUI color metadata matching our GPU RGBA->NV12 shader / Video Processor
        let (primaries, transfer, matrix) = config.color_matrix.vui_codes();
        let mut vsi = MfxExtVideoSignalInfo {
            header: MfxExtBuffer {
                buffer_id: MFX_EXTBUFF_VIDEO_SIGNAL_INFO,
                buffer_sz: std::mem::size_of::<MfxExtVideoSignalInfo>() as u32,
            },
            video_format: 5, // unspecified
            video_full_range: match config.color_range {
                ColorRange::Full => 1,
                ColorRange::Limited => 0,
            },
            colour_description_present: 1,
            colour_primaries: primaries,
            transfer_characteristics: transfer,
            matrix_coefficients: matrix,
        };
        let mut ext_ptrs: [*mut MfxExtBuffer; 1] =
            [&mut vsi as *mut MfxExtVideoSignalInfo as *mut MfxExtBuffer];

        let lp_candidates: &[u16] = match config.low_power {
            QsvLowPowerMode::On => &[MFX_CODINGOPTION_ON],
            QsvLowPowerMode::Off => &[MFX_CODINGOPTION_OFF],
            // Prefer fixed-function VDENC (zero 3D/EU contention with Skia rendering),
            // automatically falling back to VME where VDENC is unsupported (e.g. HEVC on Gen 9.5).
            QsvLowPowerMode::Auto => &[MFX_CODINGOPTION_ON, MFX_CODINGOPTION_OFF],
        };

        let mut chosen_param: Option<(MfxVideoParam, bool)> = None;
        let mut last_err = String::new();
        for &lp in lp_candidates {
            let mut param = build_video_param(&config, lp, Some((ext_ptrs.as_mut_ptr(), 1)));
            let st = unsafe { (mfx.enc_init)(session, &mut param) };
            if st >= 0 {
                chosen_param = Some((param, lp == MFX_CODINGOPTION_ON));
                break;
            }
            // Retry without ext video signal info in case an older driver rejects VSI extbuf
            let mut param_no_ext = build_video_param(&config, lp, None);
            let st2 = unsafe { (mfx.enc_init)(session, &mut param_no_ext) };
            if st2 >= 0 {
                chosen_param = Some((param_no_ext, lp == MFX_CODINGOPTION_ON));
                break;
            }
            last_err = format!(
                "low_power={} init failed: {st} ({}), retry without VSI: {st2} ({})",
                lp == MFX_CODINGOPTION_ON,
                mfx_status_name(st),
                mfx_status_name(st2)
            );
        }

        let Some((mut active_param, low_power_used)) = chosen_param else {
            bail!("MFXVideoENCODE_Init failed: {last_err}");
        };
        session_guard.mark_encoder_initialized();

        // Query SPS/PPS (and VPS for HEVC) so FFmpeg MP4 muxer gets valid extradata
        let extradata = query_extradata(mfx, session, config.codec, &mut active_param)?;

        // Open FFmpeg MP4 output container
        let mut output = ffmpeg::format::output(video_path)
            .with_context(|| format!("open QSV MP4 output {}", video_path.display()))?;
        let codec_id = match config.codec {
            VideoCodec::H264 => ffmpeg::ffi::AVCodecID::AV_CODEC_ID_H264,
            VideoCodec::Hevc => ffmpeg::ffi::AVCodecID::AV_CODEC_ID_HEVC,
        };
        let stream_index = unsafe {
            let fmt_ptr = output.as_mut_ptr();
            let stream = ffmpeg::ffi::avformat_new_stream(fmt_ptr, std::ptr::null());
            if stream.is_null() {
                bail!("avformat_new_stream failed for QSV MP4 output");
            }
            (*stream).time_base = ffmpeg::ffi::AVRational {
                num: 1,
                den: config.fps as i32,
            };
            (*stream).avg_frame_rate = ffmpeg::ffi::AVRational {
                num: config.fps as i32,
                den: 1,
            };
            let par = (*stream).codecpar;
            (*par).codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
            (*par).codec_id = codec_id;
            (*par).format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
            (*par).width = config.width as i32;
            (*par).height = config.height as i32;
            (*par).bit_rate = config.target_kbps as i64 * 1000;
            (*par).color_range = match config.color_range {
                ColorRange::Limited => ffmpeg::ffi::AVColorRange::AVCOL_RANGE_MPEG,
                ColorRange::Full => ffmpeg::ffi::AVColorRange::AVCOL_RANGE_JPEG,
            };
            (*par).color_space = match config.color_matrix {
                ColorMatrix::Bt601 => ffmpeg::ffi::AVColorSpace::AVCOL_SPC_SMPTE170M,
                ColorMatrix::Bt709 => ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT709,
                ColorMatrix::Bt2020 => ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL,
            };
            (*par).color_primaries = match config.color_matrix {
                ColorMatrix::Bt601 => ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_SMPTE170M,
                ColorMatrix::Bt709 => ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT709,
                ColorMatrix::Bt2020 => ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT2020,
            };
            (*par).color_trc = match config.color_matrix {
                ColorMatrix::Bt601 => {
                    ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE170M
                }
                ColorMatrix::Bt709 => ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709,
                ColorMatrix::Bt2020 => {
                    ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT2020_10
                }
            };
            if config.codec == VideoCodec::Hevc {
                (*par).codec_tag = u32::from_le_bytes(*b"hvc1");
            }
            if !extradata.is_empty() {
                let extra_ptr = ffmpeg::ffi::av_mallocz(
                    extradata.len() + ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
                ) as *mut u8;
                if extra_ptr.is_null() {
                    bail!("av_mallocz failed for QSV extradata");
                }
                std::ptr::copy_nonoverlapping(extradata.as_ptr(), extra_ptr, extradata.len());
                (*par).extradata = extra_ptr;
                (*par).extradata_size = extradata.len() as i32;
            }
            (*stream).index as usize
        };

        output
            .write_header()
            .context("write MP4 header for QSV output")?;
        let stream_time_base = output
            .stream(stream_index)
            .context("QSV output stream missing")?
            .time_base();

        // Allocate async bitstream ring (size = async_depth + 1).
        // QSV requires MaxLength >= BufferSizeInKB * 1000 * BRCParamMultiplier.
        let ring_depth = (config.async_depth as usize).max(1) + 1;
        let brc_mult = active_param.mfx.brc_param_multiplier.max(1) as usize;
        let mfx_required_bs = (active_param.mfx.buffer_size_in_kb as usize)
            .saturating_mul(1000)
            .saturating_mul(brc_mult);
        let bs_capacity = mfx_required_bs
            .max(config.width as usize * config.height as usize)
            .max(4 * 1024 * 1024);
        let mut bs_buffers: Vec<Vec<u8>> = (0..ring_depth).map(|_| vec![0u8; bs_capacity]).collect();
        let bitstreams: Vec<MfxBitstream> = bs_buffers
            .iter_mut()
            .map(|buf| {
                let mut bs: MfxBitstream = unsafe { std::mem::zeroed() };
                bs.data = buf.as_mut_ptr();
                bs.max_length = buf.len() as u32;
                bs
            })
            .collect();
        let free_bs_slots: VecDeque<usize> = (0..ring_depth).collect();

        eprintln!(
            "win-qsv initialized: {:?} {}x{}@{}fps | {} (MFX v{}.{}, {}) | mode={} | async_depth={} gop={} tu={}",
            config.codec,
            config.width,
            config.height,
            config.fps,
            d3d11.adapter_name,
            ver.major,
            ver.minor,
            mfx.dll_name,
            if low_power_used { "VDENC" } else { "VME" },
            config.async_depth,
            config.gop_size,
            config.target_usage,
        );

        Ok(Self {
            mfx,
            session: session_guard,
            in_flight_surfaces: Vec::with_capacity(ring_depth + 4),
            bs_buffers,
            bitstreams,
            free_bs_slots,
            pending: VecDeque::with_capacity(ring_depth),
            _allocator: allocator,
            _alloc_state: alloc_state,
            d3d11,
            frame_info: active_param.mfx.frame_info,
            config,
            low_power_used,
            output,
            stream_index,
            stream_time_base,
            packets_written: 0,
            cpu_upload: None,
        })
    }

    /// Reclaims surfaces ONLY after `MFXVideoCORE_SyncOperation` has established a formal
    /// happens-before completion boundary for that frame. Unsynchronized in-flight surfaces
    /// (`output_synced == false`) are NEVER inspected or freed.
    fn reclaim_synced_surfaces(&mut self, output_ts_90k: Option<u64>, submitted_ts_90k: Option<u64>) {
        for entry in &mut self.in_flight_surfaces {
            if output_ts_90k == Some(entry.ts_90k)
                || (self.config.gop_ref_dist <= 1 && submitted_ts_90k == Some(entry.ts_90k))
            {
                entry.output_synced = true;
            }
        }
        self.in_flight_surfaces.retain(|entry| {
            if !entry.output_synced {
                // Still unsynchronized in MFX; do not read `Data.Locked` across threads.
                return true;
            }
            // Formal `SyncOperation` barrier has completed for this frame. Defensively verify
            // `Data.Locked == 0` via `UnsafeCell` raw pointer in case the encoder retained an
            // extra lookahead/reference lock beyond output.
            unsafe { MfxFrameSurface1::read_locked_raw(entry.surface_ptr()) != 0 }
        });
    }

    fn sync_point_wait(&self, sync_point: MfxSyncPoint, stage: &str) -> Result<()> {
        let mut attempts = 0u32;
        loop {
            let st = unsafe { (self.mfx.sync_op)(self.session.session, sync_point, 10_000) };
            if st == MFX_WRN_IN_EXECUTION {
                attempts += 1;
                if attempts >= 6 {
                    let health = self.d3d11.check_device_health();
                    bail!(
                        "MFXVideoCORE_SyncOperation({stage}) timed out after 60s (MFX_WRN_IN_EXECUTION) [device health: {:?}]",
                        health
                    );
                }
                continue;
            }
            if st < 0 {
                let health = self.d3d11.check_device_health();
                bail!(
                    "MFXVideoCORE_SyncOperation({stage}) failed: {st} ({}) [device health: {:?}]",
                    mfx_status_name(st),
                    health
                );
            }
            return Ok(());
        }
    }

    fn sync_oldest(&mut self) -> Result<()> {
        let Some(item) = self.pending.pop_front() else {
            return Ok(());
        };
        let sync_res = self.sync_point_wait(item.sync_point, "encode");
        if let Err(err) = sync_res {
            self.bitstreams[item.slot].data_offset = 0;
            self.bitstreams[item.slot].data_length = 0;
            self.free_bs_slots.push_back(item.slot);
            return Err(err);
        }
        let write_res = self.write_bitstream_slot(item.slot);
        self.free_bs_slots.push_back(item.slot);
        let output_ts = match write_res {
            Ok(ts) => ts,
            Err(err) => return Err(err),
        };
        self.reclaim_synced_surfaces(output_ts, Some(item.submitted_ts_90k));
        Ok(())
    }

    fn write_bitstream_slot(&mut self, slot: usize) -> Result<Option<u64>> {
        let bs = &mut self.bitstreams[slot];
        let len = bs.data_length as usize;
        let output_ts = if len > 0 || bs.timestamp != MFX_TIMESTAMP_UNKNOWN as u64 {
            Some(bs.timestamp)
        } else {
            None
        };
        if len > 0 {
            let offset = bs.data_offset as usize;
            let data = &self.bs_buffers[slot][offset..offset + len];
            let is_keyframe = (bs.frame_type & (MFX_FRAMETYPE_IDR | MFX_FRAMETYPE_I)) != 0;
            let fps = self.config.fps as i64;
            let pts_90k = bs.timestamp as i64;
            let mut packet = ffmpeg::Packet::copy(data);
            packet.set_stream(self.stream_index);
            if self.config.gop_ref_dist <= 1 {
                // Recover exact frame index in 1/fps timebase for zero rounding drift.
                let frame_idx =
                    ((pts_90k as i128 * fps as i128 + (MFX_TIMEBASE as i128 / 2))
                        / MFX_TIMEBASE as i128) as i64;
                packet.set_pts(Some(frame_idx));
                packet.set_dts(Some(frame_idx));
                packet.set_duration(1);
                let src_tb = ffmpeg::Rational(1, self.config.fps as i32);
                packet.rescale_ts(src_tb, self.stream_time_base);
            } else {
                // Both TimeStamp and DecodeTimeStamp from MFX are in 90 kHz clock ticks.
                let dts_90k = if bs.decode_timestamp == MFX_TIMESTAMP_UNKNOWN
                    || bs.decode_timestamp == i64::MIN
                    || bs.decode_timestamp > pts_90k
                {
                    pts_90k
                } else {
                    bs.decode_timestamp
                };
                packet.set_pts(Some(pts_90k));
                packet.set_dts(Some(dts_90k));
                packet.set_duration((MFX_TIMEBASE / fps).max(1));
                let src_tb = ffmpeg::Rational(1, MFX_TIMEBASE as i32);
                packet.rescale_ts(src_tb, self.stream_time_base);
            }
            if is_keyframe {
                packet.set_flags(ffmpeg::packet::Flags::KEY);
            }
            packet
                .write_interleaved(&mut self.output)
                .context("write QSV video packet to MP4")?;
            self.packets_written += 1;
        }
        bs.data_offset = 0;
        bs.data_length = 0;
        Ok(output_ts)
    }

    fn encode_lease(&mut self, pts: i64, lease: Nv12SurfaceLease) -> Result<()> {
        // Maintain async queue depth and guarantee a free bitstream slot without collisions.
        while self.pending.len() >= self.config.async_depth as usize
            || self.free_bs_slots.is_empty()
        {
            self.sync_oldest()?;
        }

        let fps = self.config.fps as i128;
        let ts_90k = ((pts as i128 * MFX_TIMEBASE as i128 + (fps / 2)) / fps) as u64;

        let mut surface: MfxFrameSurface1 = unsafe { std::mem::zeroed() };
        surface.info = self.frame_info;
        surface.data.mem_id = lease.mem_id();
        surface.data.timestamp = ts_90k;
        surface.data.frame_order = pts as u32;

        // Keep `boxed` locally during MFX_WRN_DEVICE_BUSY retries so `sync_oldest()`
        // cannot touch or free it before MFX accepts the surface.
        let boxed = Box::new(InFlightSurface {
            surface: UnsafeCell::new(surface),
            ts_90k,
            output_synced: false,
            _lease: lease,
        });
        let bs_slot = self
            .free_bs_slots
            .pop_front()
            .context("no free QSV bitstream slot")?;

        loop {
            let surf_ptr: *mut MfxFrameSurface1 = boxed.surface_ptr();
            let bs_ptr: *mut MfxBitstream = &mut self.bitstreams[bs_slot];
            let mut sync_point: MfxSyncPoint = null_mut();
            let st = unsafe {
                (self.mfx.enc_frame_async)(
                    self.session.session,
                    null_mut(),
                    surf_ptr,
                    bs_ptr,
                    &mut sync_point,
                )
            };
            if st == MFX_WRN_DEVICE_BUSY {
                if !self.pending.is_empty() {
                    if let Err(err) = self.sync_oldest() {
                        self.free_bs_slots.push_back(bs_slot);
                        return Err(err);
                    }
                } else {
                    std::thread::sleep(std::time::Duration::from_micros(100));
                }
                continue;
            }
            if st == MFX_ERR_MORE_DATA || st == MFX_ERR_MORE_SURFACE {
                // Accepted and buffered for B-frame reordering; keep alive until its
                // later `SyncOperation` outputs `ts_90k`. Return unused bs_slot.
                self.bitstreams[bs_slot].data_offset = 0;
                self.bitstreams[bs_slot].data_length = 0;
                self.free_bs_slots.push_back(bs_slot);
                self.in_flight_surfaces.push(boxed);
                break;
            }
            if st < 0 {
                self.bitstreams[bs_slot].data_offset = 0;
                self.bitstreams[bs_slot].data_length = 0;
                self.free_bs_slots.push_back(bs_slot);
                let health = self.d3d11.check_device_health();
                bail!(
                    "MFXVideoENCODE_EncodeFrameAsync failed at pts={pts}: {st} ({}) [device health: {:?}]",
                    mfx_status_name(st),
                    health
                );
            }
            if !sync_point.is_null() {
                // Always retain surface ownership until `SyncOperation` completes.
                self.in_flight_surfaces.push(boxed);
                self.pending.push_back(PendingBitstream {
                    slot: bs_slot,
                    sync_point,
                    submitted_ts_90k: ts_90k,
                });
            } else {
                self.bitstreams[bs_slot].data_offset = 0;
                self.bitstreams[bs_slot].data_length = 0;
                self.free_bs_slots.push_back(bs_slot);
                if unsafe { MfxFrameSurface1::read_locked_raw(boxed.surface_ptr()) != 0 } {
                    self.in_flight_surfaces.push(boxed);
                }
            }
            break;
        }
        Ok(())
    }

    fn encode_gpu_frame(&mut self, pts: i64, gpu_frame: WinGpuFrame) -> Result<()> {
        if !Arc::ptr_eq(gpu_frame.lease.d3d11(), &self.d3d11) {
            bail!("WinGpuFrame D3D11 device does not match QSV encoder D3D11 device");
        }
        if gpu_frame.width != self.config.width
            || gpu_frame.height != self.config.height
            || gpu_frame.aligned_width != self.frame_info.width as u32
            || gpu_frame.aligned_height != self.frame_info.height as u32
        {
            bail!(
                "WinGpuFrame dimensions {}x{} (aligned {}x{}) do not match QSV encoder {}x{} (aligned {}x{})",
                gpu_frame.width,
                gpu_frame.height,
                gpu_frame.aligned_width,
                gpu_frame.aligned_height,
                self.config.width,
                self.config.height,
                self.frame_info.width,
                self.frame_info.height,
            );
        }
        record_gpu_frame_encoded();
        self.encode_lease(pts, gpu_frame.lease)
    }

    fn encode_cpu_frame(&mut self, pts: i64, frame: &RenderedFrame) -> Result<()> {
        record_cpu_fallback_frame();
        if self.cpu_upload.is_none() {
            let pool_size = ((self.config.async_depth as usize)
                + (self.config.gop_ref_dist as usize)
                + 4)
            .max(DEFAULT_NV12_POOL_SIZE);
            self.cpu_upload = Some(CpuUploadFallback::new(
                Arc::clone(&self.d3d11),
                self.config.width,
                self.config.height,
                pool_size,
                self.config.color_matrix,
                self.config.color_range,
            )?);
        }
        let lease = self
            .cpu_upload
            .as_ref()
            .unwrap()
            .upload_and_convert(frame.pixels(), frame.row_bytes())?;
        self.encode_lease(pts, lease)
    }

    fn finish(&mut self) -> Result<()> {
        // 1. Drain already-queued sync points
        while !self.pending.is_empty() {
            self.sync_oldest()?;
        }

        // 2. Flush buffered frames inside QSV encoder (pass NULL input surface)
        loop {
            let bs_slot = self
                .free_bs_slots
                .pop_front()
                .context("no free QSV bitstream slot during flush")?;
            let bs_ptr: *mut MfxBitstream = &mut self.bitstreams[bs_slot];
            let mut sync_point: MfxSyncPoint = null_mut();
            let st = unsafe {
                (self.mfx.enc_frame_async)(
                    self.session.session,
                    null_mut(),
                    null_mut(),
                    bs_ptr,
                    &mut sync_point,
                )
            };
            if st == MFX_ERR_MORE_DATA {
                self.bitstreams[bs_slot].data_offset = 0;
                self.bitstreams[bs_slot].data_length = 0;
                self.free_bs_slots.push_back(bs_slot);
                break;
            }
            if st == MFX_WRN_DEVICE_BUSY {
                self.free_bs_slots.push_back(bs_slot);
                std::thread::sleep(std::time::Duration::from_micros(200));
                continue;
            }
            if st < 0 {
                self.bitstreams[bs_slot].data_offset = 0;
                self.bitstreams[bs_slot].data_length = 0;
                self.free_bs_slots.push_back(bs_slot);
                bail!(
                    "MFXVideoENCODE_EncodeFrameAsync(flush) failed: {st} ({})",
                    mfx_status_name(st)
                );
            }
            if !sync_point.is_null() {
                let wait_res = self.sync_point_wait(sync_point, "flush");
                if let Err(err) = wait_res {
                    self.bitstreams[bs_slot].data_offset = 0;
                    self.bitstreams[bs_slot].data_length = 0;
                    self.free_bs_slots.push_back(bs_slot);
                    return Err(err);
                }
                let write_res = self.write_bitstream_slot(bs_slot);
                self.free_bs_slots.push_back(bs_slot);
                let output_ts = write_res?;
                self.reclaim_synced_surfaces(output_ts, None);
            } else {
                self.bitstreams[bs_slot].data_offset = 0;
                self.bitstreams[bs_slot].data_length = 0;
                self.free_bs_slots.push_back(bs_slot);
            }
        }

        // Close MFX encoder/session BEFORE releasing any remaining in-flight surface leases.
        self.session.close_now();
        self.in_flight_surfaces.clear();
        self.output
            .write_trailer()
            .context("write MP4 trailer for QSV output")?;
        let _ = self.low_power_used;
        Ok(())
    }
}

impl Drop for QsvSessionEncoder {
    fn drop(&mut self) {
        // 1. Drain any remaining sync points so MFX doesn't write to freed bitstream buffers
        while let Some(item) = self.pending.pop_front() {
            unsafe {
                let _ = (self.mfx.sync_op)(self.session.session, item.sync_point, 5_000);
            }
        }
        // 2. Close MFX encoder & session BEFORE freeing any MfxFrameSurface1 / NV12 leases
        self.session.close_now();
        // 3. Now safe to release all surface leases back to the pool
        self.in_flight_surfaces.clear();
    }
}

fn query_extradata(
    mfx: &MfxRuntime,
    session: MfxSession,
    codec: VideoCodec,
    active_param: &mut MfxVideoParam,
) -> Result<Vec<u8>> {
    let mut sps_buf = [0u8; 512];
    let mut pps_buf = [0u8; 512];
    let mut vps_buf = [0u8; 512];

    let mut spspps = MfxExtCodingOptionSpsPps {
        header: MfxExtBuffer {
            buffer_id: MFX_EXTBUFF_CODING_OPTION_SPSPPS,
            buffer_sz: std::mem::size_of::<MfxExtCodingOptionSpsPps>() as u32,
        },
        sps_buffer: sps_buf.as_mut_ptr(),
        pps_buffer: pps_buf.as_mut_ptr(),
        sps_buf_size: sps_buf.len() as u16,
        pps_buf_size: pps_buf.len() as u16,
        sps_id: 0,
        pps_id: 0,
    };
    let mut vps = MfxExtCodingOptionVps {
        header: MfxExtBuffer {
            buffer_id: MFX_EXTBUFF_CODING_OPTION_VPS,
            buffer_sz: std::mem::size_of::<MfxExtCodingOptionVps>() as u32,
        },
        vps_buffer: vps_buf.as_mut_ptr(),
        vps_buf_size: vps_buf.len() as u16,
        vps_id: 0,
        reserved: [0; 6],
    };

    let mut query_param = *active_param;
    let mut ext_ptrs: [*mut MfxExtBuffer; 2] = [
        &mut spspps as *mut MfxExtCodingOptionSpsPps as *mut MfxExtBuffer,
        &mut vps as *mut MfxExtCodingOptionVps as *mut MfxExtBuffer,
    ];
    let mut query_vps = codec == VideoCodec::Hevc;
    query_param.ext_param = ext_ptrs.as_mut_ptr();
    query_param.num_ext_param = if query_vps { 2 } else { 1 };

    let st = unsafe { (mfx.enc_get_video_param)(session, &mut query_param) };
    if st < 0 {
        // Fallback: query without VPS buffer so unpopulated `vps_buf` is never appended.
        query_vps = false;
        query_param.num_ext_param = 1;
        spspps.sps_buf_size = sps_buf.len() as u16;
        spspps.pps_buf_size = pps_buf.len() as u16;
        let st2 = unsafe { (mfx.enc_get_video_param)(session, &mut query_param) };
        if st2 < 0 {
            return Ok(Vec::new());
        }
    }

    let mut extradata = Vec::new();
    if query_vps && vps.vps_buf_size > 0 && (vps.vps_buf_size as usize) <= vps_buf.len() {
        extradata.extend_from_slice(&vps_buf[..vps.vps_buf_size as usize]);
    }
    if spspps.sps_buf_size > 0 && (spspps.sps_buf_size as usize) <= sps_buf.len() {
        extradata.extend_from_slice(&sps_buf[..spspps.sps_buf_size as usize]);
    }
    if spspps.pps_buf_size > 0 && (spspps.pps_buf_size as usize) <= pps_buf.len() {
        extradata.extend_from_slice(&pps_buf[..spspps.pps_buf_size as usize]);
    }
    active_param.mfx.frame_info = query_param.mfx.frame_info;
    active_param.mfx.buffer_size_in_kb = query_param.mfx.buffer_size_in_kb;
    active_param.mfx.brc_param_multiplier = query_param.mfx.brc_param_multiplier;
    Ok(extradata)
}

// ============================================================================
// Async Worker Wrapper for FfmpegNextEncoder Integration
// ============================================================================

pub enum QsvWorkerItem {
    Gpu { pts: i64, gpu_frame: WinGpuFrame },
    Cpu { pts: i64, frame: RenderedFrame },
}

pub struct WinQsvAsyncEncoder {
    sender: Option<mpsc::SyncSender<QsvWorkerItem>>,
    recycled_cpu: mpsc::Receiver<RenderedFrame>,
    handle: Option<std::thread::JoinHandle<Result<()>>>,
    output_path: PathBuf,
    staging_path: Option<PathBuf>,
    audio_path: Option<PathBuf>,
    width: u32,
    height: u32,
    pts: i64,
    finished: bool,
}

impl WinQsvAsyncEncoder {
    pub fn new(
        output: &Path,
        staging: Option<PathBuf>,
        audio: Option<&Path>,
        config: QsvEncodeConfig,
    ) -> Result<Self> {
        validate_qsv_dimensions(config.width, config.height)?;
        let video_path = staging.as_deref().unwrap_or(output).to_owned();
        let width = config.width;
        let height = config.height;
        let queue_depth = (config.async_depth as usize).clamp(2, 8);

        // Initialize QSV session synchronously on the worker thread via a handshake
        // channel so initialization errors immediately propagate for fallback handling!
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<()>>(1);
        let (sender, receiver) = mpsc::sync_channel::<QsvWorkerItem>(queue_depth);
        let (recycle_tx, recycled_cpu) = mpsc::sync_channel::<RenderedFrame>(1);

        let handle = std::thread::Builder::new()
            .name("win-qsv-encoder".to_owned())
            .spawn(move || -> Result<()> {
                let mut enc = match QsvSessionEncoder::new(&video_path, config) {
                    Ok(enc) => {
                        let _ = init_tx.send(Ok(()));
                        enc
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        let _ = init_tx.send(Err(e));
                        bail!("{msg}");
                    }
                };
                loop {
                    let next_item = if !enc.pending.is_empty() {
                        // Never block on `receiver` while holding pending QSV leases:
                        // drain completed frames to release NV12 surfaces back to the pool
                        // and eliminate circular pool-acquire deadlocks.
                        match receiver.try_recv() {
                            Ok(item) => Some(item),
                            Err(mpsc::TryRecvError::Empty) => {
                                enc.sync_oldest()?;
                                continue;
                            }
                            Err(mpsc::TryRecvError::Disconnected) => None,
                        }
                    } else {
                        receiver.recv().ok()
                    };
                    let Some(item) = next_item else {
                        break;
                    };
                    match item {
                        QsvWorkerItem::Gpu { pts, gpu_frame } => {
                            enc.encode_gpu_frame(pts, gpu_frame)?;
                        }
                        QsvWorkerItem::Cpu { pts, frame } => {
                            let res = enc.encode_cpu_frame(pts, &frame);
                            let _ = recycle_tx.try_send(frame);
                            res?;
                        }
                    }
                }
                enc.finish()
            })
            .context("spawn win-qsv-encoder worker thread")?;

        match init_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                let _ = handle.join();
                return Err(err);
            }
            Err(_) => {
                let _ = handle.join();
                bail!("win-qsv-encoder thread exited during initialization");
            }
        }

        Ok(Self {
            sender: Some(sender),
            recycled_cpu,
            handle: Some(handle),
            output_path: output.to_owned(),
            staging_path: staging,
            audio_path: audio.map(Path::to_owned),
            width,
            height,
            pts: 0,
            finished: false,
        })
    }

    fn take_worker_failure(&mut self, fallback_msg: &str) -> anyhow::Error {
        self.sender.take();
        if let Some(handle) = self.handle.take() {
            match handle.join() {
                Ok(Err(err)) => return err.context("win-qsv-encoder worker failed"),
                Ok(Ok(())) => return anyhow::anyhow!("{fallback_msg}: worker exited unexpectedly"),
                Err(_) => return anyhow::anyhow!("{fallback_msg}: worker panicked"),
            }
        }
        anyhow::anyhow!("{fallback_msg}")
    }

    pub fn submit_frame(&mut self, mut frame: RenderedFrame) -> Result<RenderedFrame> {
        if self.finished {
            bail!("WinQsvAsyncEncoder is already finished");
        }
        let pts = self.pts;
        self.pts += 1;
        let sender = self
            .sender
            .as_ref()
            .context("win-qsv-encoder worker channel closed")?;
        if let Some(gpu_frame) = frame.take_win_gpu_frame() {
            if sender
                .send(QsvWorkerItem::Gpu { pts, gpu_frame })
                .is_err()
            {
                return Err(self.take_worker_failure("send GPU frame to win-qsv-encoder"));
            }
            return Ok(frame);
        }
        // CPU fallback (e.g. raster backend or non-GPU frame)
        let expected_row = self.width as usize * 4;
        if frame.width() != self.width
            || frame.height() != self.height
            || frame.pixels().len() != self.height as usize * expected_row
        {
            bail!(
                "invalid frame dimensions or pixel buffer: expected {}x{}",
                self.width,
                self.height
            );
        }
        if sender.send(QsvWorkerItem::Cpu { pts, frame }).is_err() {
            return Err(self.take_worker_failure("send CPU frame to win-qsv-encoder"));
        }
        loop {
            match self
                .recycled_cpu
                .recv_timeout(std::time::Duration::from_millis(10))
            {
                Ok(recycled) => return Ok(recycled),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self
                        .handle
                        .as_ref()
                        .is_some_and(std::thread::JoinHandle::is_finished)
                    {
                        return Err(self.take_worker_failure(
                            "win-qsv-encoder worker stopped before recycling frame",
                        ));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.take_worker_failure(
                        "win-qsv-encoder worker stopped before recycling frame",
                    ));
                }
            }
        }
    }

    pub fn finish(&mut self) -> Result<()> {
        if self.finished {
            bail!("WinQsvAsyncEncoder already finished");
        }
        self.finished = true;
        self.sender.take();
        if let Some(handle) = self.handle.take() {
            match handle.join() {
                Ok(result) => result?,
                Err(_) => bail!("win-qsv-encoder worker thread panicked"),
            }
        }
        if let (Some(staging), Some(audio)) = (&self.staging_path, &self.audio_path) {
            mux_audio(staging, audio, &self.output_path).with_context(|| {
                format!("mux staged QSV video into {}", self.output_path.display())
            })?;
            std::fs::remove_file(staging)
                .with_context(|| format!("remove staged QSV video {}", staging.display()))?;
        }
        Ok(())
    }
}

impl Drop for WinQsvAsyncEncoder {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if let Some(staging) = &self.staging_path {
            let _ = std::fs::remove_file(staging);
        }
    }
}

fn mux_audio(staging: &Path, audio: &Path, output_path: &Path) -> Result<()> {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(staging)
        .args(["-i"])
        .arg(audio)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
            "-shortest",
        ])
        .arg(output_path);
    let display = format!("{command:?}");
    let result = command
        .output()
        .with_context(|| format!("run audio mux command: {display}"))?;
    if !result.status.success() {
        bail!(
            "audio mux failed ({})\ncommand: {display}\nstderr: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(())
}
