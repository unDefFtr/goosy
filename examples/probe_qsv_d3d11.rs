#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("probe_qsv_d3d11 is Windows-only");
}

#[cfg(target_os = "windows")]
mod win_probe {
    use anyhow::{Context, Result, bail};
    use std::ffi::{CString, c_void};
    use std::ptr::null_mut;
    use std::time::Instant;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE, RECT};
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
        D3D_SRV_DIMENSION_TEXTURE2D,
    };
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::{Interface, PCSTR, PCWSTR};

    // ---- Exact oneVPL / Intel Media SDK x64 ABI structures ----
    type MfxStatus = i32;
    type MfxSession = *mut c_void;
    type MfxSyncPoint = *mut c_void;
    type MfxMemId = *mut c_void;
    type MfxHdl = *mut c_void;

    const MFX_ERR_NONE: MfxStatus = 0;
    const MFX_ERR_MORE_DATA: MfxStatus = -10;
    const MFX_ERR_MORE_SURFACE: MfxStatus = -11;
    const MFX_WRN_IN_EXECUTION: MfxStatus = 1;
    const MFX_WRN_DEVICE_BUSY: MfxStatus = 2;

    const MFX_IMPL_HARDWARE_ANY: u32 = 0x4;
    const MFX_IMPL_VIA_D3D11: u32 = 0x300;
    const MFX_HANDLE_D3D11_DEVICE: i32 = 3;
    const MFX_CODEC_AVC: u32 = 0x20435641;
    const MFX_CODEC_HEVC: u32 = 0x43564548;
    const MFX_FOURCC_NV12: u32 = 0x3231564E;
    const MFX_CHROMAFORMAT_YUV420: u16 = 1;
    const MFX_PICSTRUCT_PROGRESSIVE: u16 = 1;
    const MFX_IOPATTERN_IN_VIDEO_MEMORY: u16 = 0x1;
    const MFX_MEMTYPE_VIDEO_MEMORY_DECODER_TARGET: u16 = 0x10;
    const MFX_MEMTYPE_FROM_ENCODE: u16 = 0x100;
    const MFX_MEMTYPE_EXTERNAL_FRAME: u16 = 0x2;
    const MFX_RATECONTROL_VBR: u16 = 2;
    const MFX_CODINGOPTION_ON: u16 = 0x10;
    const MFX_CODINGOPTION_OFF: u16 = 0x20;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct MfxVersion {
        minor: u16,
        major: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxInitParam {
        implementation: u32,
        version: MfxVersion,
        external_threads: u16,
        _pad0: [u8; 6],
        ext_param: *mut *mut MfxExtBuffer,
        num_ext_param: u16,
        _pad1: [u8; 6],
        gpu_copy: u16,
        reserved: [u16; 23],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct MfxExtBuffer {
        buffer_id: u32,
        buffer_sz: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxFrameInfo {
        reserved: [u32; 4],
        channel_id: u16,
        bit_depth_luma: u16,
        bit_depth_chroma: u16,
        shift: u16,
        frame_id: [u16; 4],
        fourcc: u32,
        width: u16,
        height: u16,
        crop_x: u16,
        crop_y: u16,
        crop_w: u16,
        crop_h: u16,
        frame_rate_ext_n: u32,
        frame_rate_ext_d: u32,
        reserved3: u16,
        aspect_ratio_w: u16,
        aspect_ratio_h: u16,
        pic_struct: u16,
        chroma_format: u16,
        reserved2: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxFrameData {
        ext_param: *mut *mut MfxExtBuffer,
        num_ext_param: u16,
        reserved: [u16; 9],
        mem_type: u16,
        pitch_high: u16,
        timestamp: u64,
        frame_order: u32,
        locked: u16,
        pitch_low: u16,
        y: *mut u8,
        uv: *mut u8,
        v: *mut u8,
        a: *mut u8,
        mem_id: MfxMemId,
        corrupted: u16,
        data_flag: u16,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxFrameSurface1 {
        frame_interface: *mut c_void,
        reserved: [u32; 2],
        info: MfxFrameInfo,
        _pad: u32,
        data: MfxFrameData,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxInfoMfx {
        reserved: [u32; 7],
        low_power: u16,
        brc_param_multiplier: u16,
        frame_info: MfxFrameInfo,
        codec_id: u32,
        codec_profile: u16,
        codec_level: u16,
        num_thread: u16,
        target_usage: u16,
        gop_pic_size: u16,
        gop_ref_dist: u16,
        gop_opt_flag: u16,
        idr_interval: u16,
        rate_control_method: u16,
        initial_delay_in_kb: u16,
        buffer_size_in_kb: u16,
        target_kbps: u16,
        max_kbps: u16,
        num_slice: u16,
        num_ref_frame: u16,
        encoded_order: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxVideoParam {
        alloc_id: u32,
        reserved: [u32; 2],
        reserved3: u16,
        async_depth: u16,
        mfx: MfxInfoMfx,
        _union_pad: [u32; 8],
        protected: u16,
        io_pattern: u16,
        _pad0: u32,
        ext_param: *mut *mut MfxExtBuffer,
        num_ext_param: u16,
        reserved2: u16,
        _pad1: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxBitstream {
        encrypted_data: *mut c_void,
        ext_param: *mut *mut MfxExtBuffer,
        num_ext_param: u16,
        reserved: [u16; 3],
        decode_timestamp: i64,
        timestamp: u64,
        data: *mut u8,
        data_offset: u32,
        data_length: u32,
        max_length: u32,
        pic_struct: u16,
        frame_type: u16,
        data_flag: u16,
        reserved2: u16,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxFrameAllocRequest {
        alloc_id: u32,
        reserved: [u32; 3],
        info: MfxFrameInfo,
        mem_type: u16,
        num_frame_min: u16,
        num_frame_suggested: u16,
        reserved2: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxFrameAllocResponse {
        alloc_id: u32,
        reserved: [u32; 3],
        mids: *mut MfxMemId,
        num_frame_actual: u16,
        reserved2: [u16; 3],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxHdlPair {
        first: MfxHdl,
        second: MfxHdl,
    }

    type FnAlloc = unsafe extern "C" fn(
        *mut c_void,
        *mut MfxFrameAllocRequest,
        *mut MfxFrameAllocResponse,
    ) -> MfxStatus;
    type FnLock = unsafe extern "C" fn(*mut c_void, MfxMemId, *mut MfxFrameData) -> MfxStatus;
    type FnUnlock = unsafe extern "C" fn(*mut c_void, MfxMemId, *mut MfxFrameData) -> MfxStatus;
    type FnGetHdl = unsafe extern "C" fn(*mut c_void, MfxMemId, *mut MfxHdl) -> MfxStatus;
    type FnFree = unsafe extern "C" fn(*mut c_void, *mut MfxFrameAllocResponse) -> MfxStatus;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MfxFrameAllocator {
        reserved: [u32; 4],
        pthis: *mut c_void,
        alloc: Option<FnAlloc>,
        lock: Option<FnLock>,
        unlock: Option<FnUnlock>,
        get_hdl: Option<FnGetHdl>,
        free: Option<FnFree>,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct MfxPlatform {
        code_name: u16,
        device_id: u16,
        media_adapter_type: u16,
        reserved: [u16; 13],
    }

    const _: () = {
        assert!(std::mem::size_of::<MfxInitParam>() == 80);
        assert!(std::mem::size_of::<MfxFrameInfo>() == 68);
        assert!(std::mem::size_of::<MfxFrameData>() == 96);
        assert!(std::mem::size_of::<MfxFrameSurface1>() == 184);
        assert!(std::mem::size_of::<MfxInfoMfx>() == 136);
        assert!(std::mem::size_of::<MfxVideoParam>() == 208);
        assert!(std::mem::size_of::<MfxBitstream>() == 72);
        assert!(std::mem::size_of::<MfxFrameAllocRequest>() == 92);
        assert!(std::mem::size_of::<MfxFrameAllocResponse>() == 32);
        assert!(std::mem::size_of::<MfxFrameAllocator>() == 64);
        assert!(std::mem::size_of::<MfxHdlPair>() == 16);
        assert!(std::mem::size_of::<MfxPlatform>() == 32);
    };

    type FnMfxInitEx = unsafe extern "C" fn(MfxInitParam, *mut MfxSession) -> MfxStatus;
    type FnMfxClose = unsafe extern "C" fn(MfxSession) -> MfxStatus;
    type FnMfxQueryVersion = unsafe extern "C" fn(MfxSession, *mut MfxVersion) -> MfxStatus;
    type FnMfxQueryImpl = unsafe extern "C" fn(MfxSession, *mut u32) -> MfxStatus;
    type FnMfxQueryPlatform = unsafe extern "C" fn(MfxSession, *mut MfxPlatform) -> MfxStatus;
    type FnMfxSetHandle = unsafe extern "C" fn(MfxSession, i32, MfxHdl) -> MfxStatus;
    type FnMfxSetFrameAllocator =
        unsafe extern "C" fn(MfxSession, *mut MfxFrameAllocator) -> MfxStatus;
    type FnMfxSyncOperation = unsafe extern "C" fn(MfxSession, MfxSyncPoint, u32) -> MfxStatus;
    type FnMfxEncodeQueryIoSurf = unsafe extern "C" fn(
        MfxSession,
        *mut MfxVideoParam,
        *mut MfxFrameAllocRequest,
    ) -> MfxStatus;
    type FnMfxEncodeInit = unsafe extern "C" fn(MfxSession, *mut MfxVideoParam) -> MfxStatus;
    type FnMfxEncodeClose = unsafe extern "C" fn(MfxSession) -> MfxStatus;
    type FnMfxEncodeFrameAsync = unsafe extern "C" fn(
        MfxSession,
        *mut c_void,
        *mut MfxFrameSurface1,
        *mut MfxBitstream,
        *mut MfxSyncPoint,
    ) -> MfxStatus;

    struct MfxLib {
        _module: HMODULE,
        init_ex: FnMfxInitEx,
        close: FnMfxClose,
        query_version: FnMfxQueryVersion,
        query_impl: FnMfxQueryImpl,
        query_platform: FnMfxQueryPlatform,
        set_handle: FnMfxSetHandle,
        set_allocator: FnMfxSetFrameAllocator,
        sync_op: FnMfxSyncOperation,
        enc_query_iosurf: FnMfxEncodeQueryIoSurf,
        enc_init: FnMfxEncodeInit,
        enc_close: FnMfxEncodeClose,
        enc_frame_async: FnMfxEncodeFrameAsync,
    }

    impl MfxLib {
        fn load(dll_name: &str) -> Result<Self> {
            let wide: Vec<u16> = dll_name.encode_utf16().chain(std::iter::once(0)).collect();
            let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
                .with_context(|| format!("LoadLibraryW({dll_name})"))?;
            macro_rules! sym {
                ($name:literal, $ty:ty) => {{
                    let c = CString::new($name).unwrap();
                    let p = unsafe { GetProcAddress(module, PCSTR(c.as_ptr() as *const u8)) }
                        .with_context(|| format!("missing symbol {} in {}", $name, dll_name))?;
                    unsafe { std::mem::transmute::<_, $ty>(p) }
                }};
            }
            Ok(Self {
                _module: module,
                init_ex: sym!("MFXInitEx", FnMfxInitEx),
                close: sym!("MFXClose", FnMfxClose),
                query_version: sym!("MFXQueryVersion", FnMfxQueryVersion),
                query_impl: sym!("MFXQueryIMPL", FnMfxQueryImpl),
                query_platform: sym!("MFXVideoCORE_QueryPlatform", FnMfxQueryPlatform),
                set_handle: sym!("MFXVideoCORE_SetHandle", FnMfxSetHandle),
                set_allocator: sym!("MFXVideoCORE_SetFrameAllocator", FnMfxSetFrameAllocator),
                sync_op: sym!("MFXVideoCORE_SyncOperation", FnMfxSyncOperation),
                enc_query_iosurf: sym!("MFXVideoENCODE_QueryIOSurf", FnMfxEncodeQueryIoSurf),
                enc_init: sym!("MFXVideoENCODE_Init", FnMfxEncodeInit),
                enc_close: sym!("MFXVideoENCODE_Close", FnMfxEncodeClose),
                enc_frame_async: sym!("MFXVideoENCODE_EncodeFrameAsync", FnMfxEncodeFrameAsync),
            })
        }
    }

    struct AllocEntry {
        _textures: Vec<ID3D11Texture2D>,
        hdl_pairs: Vec<MfxHdlPair>,
        mids: Vec<MfxMemId>,
    }

    struct D3d11AllocState {
        device: ID3D11Device,
        allocs: Vec<Box<AllocEntry>>,
    }

    unsafe extern "C" fn alloc_cb(
        pthis: *mut c_void,
        request: *mut MfxFrameAllocRequest,
        response: *mut MfxFrameAllocResponse,
    ) -> MfxStatus {
        let state = unsafe { &mut *(pthis as *mut D3d11AllocState) };
        let req = unsafe { &*request };
        let count = req.num_frame_suggested.max(req.num_frame_min).max(1) as usize;
        // Try BindFlags combinations that work for individual ArraySize=1 NV12 textures on Intel UHD 630
        // (and fallback to D3D11_BIND_DECODER for internal MFX surfaces if needed).
        let is_external = (req.mem_type & MFX_MEMTYPE_EXTERNAL_FRAME) != 0;
        let bind_candidates: &[u32] = if is_external {
            &[
                (D3D11_BIND_RENDER_TARGET.0
                    | D3D11_BIND_UNORDERED_ACCESS.0
                    | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                D3D11_BIND_DECODER.0 as u32,
            ]
        } else {
            &[
                D3D11_BIND_DECODER.0 as u32,
                (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            ]
        };

        let mut textures = Vec::with_capacity(count);
        let mut hdl_pairs = Vec::with_capacity(count);
        for _ in 0..count {
            let mut created: Option<ID3D11Texture2D> = None;
            for &flags in bind_candidates {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: req.info.width as u32,
                    Height: req.info.height as u32,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
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
                return -4; // MFX_ERR_MEMORY_ALLOC
            };
            hdl_pairs.push(MfxHdlPair {
                first: tex.as_raw(),
                second: null_mut(),
            });
            textures.push(tex);
        }

        let mut entry = Box::new(AllocEntry {
            _textures: textures,
            hdl_pairs,
            mids: Vec::with_capacity(count),
        });
        for i in 0..count {
            let p = &mut entry.hdl_pairs[i] as *mut MfxHdlPair as MfxMemId;
            entry.mids.push(p);
        }
        let resp = unsafe { &mut *response };
        resp.mids = entry.mids.as_mut_ptr();
        resp.num_frame_actual = count as u16;
        state.allocs.push(entry);
        MFX_ERR_NONE
    }

    unsafe extern "C" fn lock_cb(_: *mut c_void, _: MfxMemId, _: *mut MfxFrameData) -> MfxStatus {
        -3 // MFX_ERR_UNSUPPORTED (pure video memory)
    }
    unsafe extern "C" fn unlock_cb(_: *mut c_void, _: MfxMemId, _: *mut MfxFrameData) -> MfxStatus {
        MFX_ERR_NONE
    }
    unsafe extern "C" fn get_hdl_cb(
        _pthis: *mut c_void,
        mid: MfxMemId,
        handle: *mut MfxHdl,
    ) -> MfxStatus {
        unsafe {
            let pair = &*(mid as *const MfxHdlPair);
            let out = &mut *(handle as *mut MfxHdlPair);
            *out = *pair;
        }
        MFX_ERR_NONE
    }
    unsafe extern "C" fn free_cb(_: *mut c_void, _: *mut MfxFrameAllocResponse) -> MfxStatus {
        MFX_ERR_NONE
    }

    const HLSL_RGBA_TO_NV12: &str = r#"
Texture2D<float4> srcTex : register(t0);
RWTexture2D<unorm float> dstY : register(u0);
RWTexture2D<unorm float2> dstUV : register(u1);

cbuffer Params : register(b0) {
    uint width;
    uint height;
    uint _pad0;
    uint _pad1;
};

[numthreads(16, 16, 1)]
void main(uint3 gid : SV_DispatchThreadID) {
    uint x = gid.x * 2;
    uint y = gid.y * 2;
    if (x >= width || y >= height) return;

    float3 p00 = srcTex.Load(int3(x, y, 0)).rgb;
    float3 p10 = srcTex.Load(int3(min(x + 1, width - 1), y, 0)).rgb;
    float3 p01 = srcTex.Load(int3(x, min(y + 1, height - 1), 0)).rgb;
    float3 p11 = srcTex.Load(int3(min(x + 1, width - 1), min(y + 1, height - 1), 0)).rgb;

    const float3 y_coeff = float3(0.25678824, 0.50412941, 0.09790588);
    const float y_off = 16.0 / 255.0;

    dstY[uint2(x, y)] = dot(p00, y_coeff) + y_off;
    if (x + 1 < width) dstY[uint2(x + 1, y)] = dot(p10, y_coeff) + y_off;
    if (y + 1 < height) dstY[uint2(x, y + 1)] = dot(p01, y_coeff) + y_off;
    if (x + 1 < width && y + 1 < height) dstY[uint2(x + 1, y + 1)] = dot(p11, y_coeff) + y_off;

    float3 avg = 0.25 * (p00 + p10 + p01 + p11);
    const float3 u_coeff = float3(-0.14822353, -0.29099216, 0.43921569);
    const float3 v_coeff = float3(0.43921569, -0.36778824, -0.07142745);
    dstUV[gid.xy] = float2(dot(avg, u_coeff) + (128.0 / 255.0), dot(avg, v_coeff) + (128.0 / 255.0));
}
"#;

    pub fn run() -> Result<()> {
        println!("=== 1. DXGI Adapter & D3D12 / D3D11 Device Creation (Same LUID) ===");
        let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1()? };
        let adapter: IDXGIAdapter1 = unsafe { factory.EnumAdapters1(0)? };
        let desc = unsafe { adapter.GetDesc1()? };
        let name = String::from_utf16_lossy(
            &desc.Description[..desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len())],
        );
        println!(
            "Adapter 0: {} (Vendor 0x{:04X}, Device 0x{:04X}, LUID {:08X}:{:08X})",
            name, desc.VendorId, desc.DeviceId, desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart
        );

        let mut d3d12_dev: Option<ID3D12Device> = None;
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut d3d12_dev)? };
        let d3d12_dev = d3d12_dev.unwrap();
        let d3d12_luid = unsafe { d3d12_dev.GetAdapterLuid() };
        let d3d12_queue: ID3D12CommandQueue = unsafe {
            d3d12_dev.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                ..Default::default()
            })?
        };

        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let mut d3d11_dev: Option<ID3D11Device> = None;
        let mut d3d11_ctx: Option<ID3D11DeviceContext> = None;
        let mut fl = D3D_FEATURE_LEVEL_11_0;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut d3d11_dev),
                Some(&mut fl),
                Some(&mut d3d11_ctx),
            )?;
        }
        let d3d11_dev = d3d11_dev.unwrap();
        let d3d11_ctx = d3d11_ctx.unwrap();
        let mt: ID3D11Multithread = d3d11_dev.cast()?;
        unsafe { mt.SetMultithreadProtected(true) };
        let d3d11_dev1: ID3D11Device1 = d3d11_dev.cast()?;
        let d3d11_dev5: ID3D11Device5 = d3d11_dev.cast()?;
        let d3d11_ctx4: ID3D11DeviceContext4 = d3d11_ctx.cast()?;
        println!(
            "D3D12 + D3D11 created on same LUID {:08X}:{:08X} (D3D11 FL=0x{:X})",
            d3d12_luid.HighPart, d3d12_luid.LowPart, fl.0
        );

        println!("\n=== 2. D3D12 <-> D3D11 Shared Texture & Shared Fence Interop ===");
        let width = 3840u32;
        let height = 2160u32;
        for (fmt_name, fmt) in [
            ("R8G8B8A8_UNORM", DXGI_FORMAT_R8G8B8A8_UNORM),
            ("B8G8R8A8_UNORM", DXGI_FORMAT_B8G8R8A8_UNORM),
        ] {
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
                Format: fmt,
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
                d3d12_dev.CreateCommittedResource(
                    &heap_props,
                    D3D12_HEAP_FLAG_SHARED,
                    &res_desc,
                    D3D12_RESOURCE_STATE_COMMON,
                    None,
                    &mut d3d12_res,
                )?;
            }
            let d3d12_res = d3d12_res.unwrap();
            let handle: HANDLE =
                unsafe { d3d12_dev.CreateSharedHandle(&d3d12_res, None, GENERIC_ALL.0, None)? };
            let opened: ID3D11Texture2D = unsafe { d3d11_dev1.OpenSharedResource1(handle)? };
            unsafe {
                let _ = CloseHandle(handle);
            }
            let mut d11_desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { opened.GetDesc(&mut d11_desc) };
            println!(
                "  Shared {}: D3D12 -> D3D11 OK ({}x{}, BindFlags=0x{:X})",
                fmt_name, d11_desc.Width, d11_desc.Height, d11_desc.BindFlags
            );
        }

        // Shared fence test
        let d3d12_fence: ID3D12Fence =
            unsafe { d3d12_dev.CreateFence(0, D3D12_FENCE_FLAG_SHARED)? };
        let fence_handle =
            unsafe { d3d12_dev.CreateSharedHandle(&d3d12_fence, None, GENERIC_ALL.0, None)? };
        let mut d3d11_fence: Option<ID3D11Fence> = None;
        unsafe { d3d11_dev5.OpenSharedFence(fence_handle, &mut d3d11_fence)? };
        let d3d11_fence = d3d11_fence.unwrap();
        unsafe {
            let _ = CloseHandle(fence_handle);
        }
        unsafe {
            d3d12_queue.Signal(&d3d12_fence, 1)?;
            d3d11_ctx4.Wait(&d3d11_fence, 1)?;
            d3d11_ctx4.Signal(&d3d11_fence, 2)?;
            let ev = CreateEventW(None, false, false, None)?;
            d3d12_fence.SetEventOnCompletion(2, ev)?;
            WaitForSingleObject(ev, INFINITE);
            let _ = CloseHandle(ev);
        }
        println!("  Shared Fence D3D12 <-> D3D11 GPU roundtrip: OK!");

        println!("\n=== 3. NV12 Texture BindFlags / UAV / RTV / VideoProcessor Probing ===");
        let flag_combos = [
            (
                "RT|SRV",
                D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0,
            ),
            (
                "UAV|SRV",
                D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_SHADER_RESOURCE.0,
            ),
            (
                "RT|UAV|SRV",
                D3D11_BIND_RENDER_TARGET.0
                    | D3D11_BIND_UNORDERED_ACCESS.0
                    | D3D11_BIND_SHADER_RESOURCE.0,
            ),
            (
                "RT|DECODER",
                D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_DECODER.0,
            ),
            (
                "RT|SRV|DECODER",
                D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_DECODER.0,
            ),
            (
                "RT|UAV|SRV|DECODER",
                D3D11_BIND_RENDER_TARGET.0
                    | D3D11_BIND_UNORDERED_ACCESS.0
                    | D3D11_BIND_SHADER_RESOURCE.0
                    | D3D11_BIND_DECODER.0,
            ),
            (
                "RT|VIDEO_ENCODER",
                D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0,
            ),
            (
                "UAV|VIDEO_ENCODER",
                D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_VIDEO_ENCODER.0,
            ),
        ];
        for (label, flags) in flag_combos {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: flags as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            let res = unsafe { d3d11_dev.CreateTexture2D(&desc, None, Some(&mut tex)) };
            let mut uav_ok = "n/a";
            let mut rtv_ok = "n/a";
            if let Some(t) = tex {
                if flags & D3D11_BIND_UNORDERED_ACCESS.0 != 0 {
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
                    let mut uav_y: Option<ID3D11UnorderedAccessView> = None;
                    let mut uav_uv: Option<ID3D11UnorderedAccessView> = None;
                    let r1 = unsafe {
                        d3d11_dev.CreateUnorderedAccessView(&t, Some(&y_desc), Some(&mut uav_y))
                    };
                    let r2 = unsafe {
                        d3d11_dev.CreateUnorderedAccessView(&t, Some(&uv_desc), Some(&mut uav_uv))
                    };
                    uav_ok = if r1.is_ok() && r2.is_ok() {
                        "YES (Y+UV direct UAV!)"
                    } else {
                        "NO"
                    };
                }
                if flags & D3D11_BIND_RENDER_TARGET.0 != 0 {
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
                    let mut rtv_y: Option<ID3D11RenderTargetView> = None;
                    let mut rtv_uv: Option<ID3D11RenderTargetView> = None;
                    let r1 = unsafe {
                        d3d11_dev.CreateRenderTargetView(&t, Some(&y_desc), Some(&mut rtv_y))
                    };
                    let r2 = unsafe {
                        d3d11_dev.CreateRenderTargetView(&t, Some(&uv_desc), Some(&mut rtv_uv))
                    };
                    rtv_ok = if r1.is_ok() && r2.is_ok() {
                        "YES (Y+UV RTV)"
                    } else {
                        "NO"
                    };
                }
            }
            println!(
                "  NV12 {:<18}: create={:<4} | UAV={} | RTV={}",
                label,
                if res.is_ok() { "OK" } else { "FAIL" },
                uav_ok,
                rtv_ok
            );
        }

        // Check Texture2DArray NV12 UAV too!
        {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 4,
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
            let res = unsafe { d3d11_dev.CreateTexture2D(&desc, None, Some(&mut tex)) };
            if let Some(t) = tex {
                let y_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_R8_UNORM,
                    ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Texture2DArray: D3D11_TEX2D_ARRAY_UAV {
                            MipSlice: 0,
                            FirstArraySlice: 1,
                            ArraySize: 1,
                        },
                    },
                };
                let uv_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_R8G8_UNORM,
                    ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Texture2DArray: D3D11_TEX2D_ARRAY_UAV {
                            MipSlice: 0,
                            FirstArraySlice: 1,
                            ArraySize: 1,
                        },
                    },
                };
                let mut uav_y: Option<ID3D11UnorderedAccessView> = None;
                let mut uav_uv: Option<ID3D11UnorderedAccessView> = None;
                let r1 = unsafe {
                    d3d11_dev.CreateUnorderedAccessView(&t, Some(&y_desc), Some(&mut uav_y))
                };
                let r2 = unsafe {
                    d3d11_dev.CreateUnorderedAccessView(&t, Some(&uv_desc), Some(&mut uav_uv))
                };
                println!(
                    "  NV12 Array(4) RT|UAV|SRV: create=OK | ArraySlice UAV={}",
                    if r1.is_ok() && r2.is_ok() {
                        "YES!"
                    } else {
                        "NO"
                    }
                );
            } else {
                println!("  NV12 Array(4) RT|UAV|SRV: create=FAIL ({:?})", res);
            }
        }

        println!("\n=== 4. Benchmark 4K RGBA -> NV12: Video Processor vs Compute Shader ===");
        let video_dev: ID3D11VideoDevice = d3d11_dev.cast()?;
        let video_ctx: ID3D11VideoContext = d3d11_ctx.cast()?;
        let video_ctx1: ID3D11VideoContext1 = d3d11_ctx.cast()?;
        let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
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
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let vp_enum = unsafe { video_dev.CreateVideoProcessorEnumerator(&content_desc)? };
        for (name, fmt) in [
            ("R8G8B8A8_UNORM", DXGI_FORMAT_R8G8B8A8_UNORM),
            ("B8G8R8A8_UNORM", DXGI_FORMAT_B8G8R8A8_UNORM),
            ("NV12", DXGI_FORMAT_NV12),
        ] {
            let support = unsafe { vp_enum.CheckVideoProcessorFormat(fmt)? };
            println!(
                "  VP Format {:<16}: input={} output={}",
                name,
                support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0,
                support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 != 0
            );
        }

        // Create test RGBA texture with solid green-ish color (R=64, G=192, B=128, A=255)
        let mut init_rgba = vec![0u8; (width * height * 4) as usize];
        for px in init_rgba.chunks_exact_mut(4) {
            px[0] = 64;
            px[1] = 192;
            px[2] = 128;
            px[3] = 255;
        }
        let rgba_desc = D3D11_TEXTURE2D_DESC {
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
        let sub = D3D11_SUBRESOURCE_DATA {
            pSysMem: init_rgba.as_ptr() as *const c_void,
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let mut rgba_tex: Option<ID3D11Texture2D> = None;
        unsafe { d3d11_dev.CreateTexture2D(&rgba_desc, Some(&sub), Some(&mut rgba_tex))? };
        let rgba_tex = rgba_tex.unwrap();

        // Create NV12 target texture
        let nv12_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
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
        let mut nv12_tex: Option<ID3D11Texture2D> = None;
        let nv12_has_uav =
            unsafe { d3d11_dev.CreateTexture2D(&nv12_desc, None, Some(&mut nv12_tex)) }.is_ok();
        let nv12_tex = if let Some(t) = nv12_tex {
            t
        } else {
            let mut d = nv12_desc;
            d.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
            let mut t = None;
            unsafe { d3d11_dev.CreateTexture2D(&d, None, Some(&mut t))? };
            t.unwrap()
        };

        // Benchmark Video Processor 4K
        {
            let vp = unsafe { video_dev.CreateVideoProcessor(&vp_enum, 0)? };
            let in_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
            unsafe {
                video_dev.CreateVideoProcessorInputView(
                    &rgba_tex,
                    &vp_enum,
                    &in_view_desc,
                    Some(&mut in_view),
                )?;
            }
            let in_view = in_view.unwrap();
            let out_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
            unsafe {
                video_dev.CreateVideoProcessorOutputView(
                    &nv12_tex,
                    &vp_enum,
                    &out_view_desc,
                    Some(&mut out_view),
                )?;
            }
            let out_view = out_view.unwrap();
            let rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            unsafe {
                video_ctx.VideoProcessorSetStreamFrameFormat(
                    &vp,
                    0,
                    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                );
                video_ctx.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&rect));
                video_ctx.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&rect));
                video_ctx.VideoProcessorSetOutputTargetRect(&vp, true, Some(&rect));
                video_ctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
                video_ctx1.VideoProcessorSetStreamColorSpace1(
                    &vp,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                video_ctx1.VideoProcessorSetOutputColorSpace1(
                    &vp,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(in_view.clone())),
                ..Default::default()
            };
            // Warmup + GPU timing with fence
            let mut sync_fence: Option<ID3D11Fence> = None;
            unsafe { d3d11_dev5.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut sync_fence)? };
            let sync_fence = sync_fence.unwrap();
            let ev = unsafe { CreateEventW(None, false, false, None)? };
            let mut fv = 0u64;
            for _ in 0..5 {
                unsafe {
                    video_ctx.VideoProcessorBlt(&vp, &out_view, 0, std::slice::from_ref(&stream))?;
                    fv += 1;
                    d3d11_ctx4.Signal(&sync_fence, fv)?;
                    sync_fence.SetEventOnCompletion(fv, ev)?;
                    WaitForSingleObject(ev, INFINITE);
                }
            }
            let iters = 100;
            let t0 = Instant::now();
            for _ in 0..iters {
                unsafe {
                    video_ctx.VideoProcessorBlt(&vp, &out_view, 0, std::slice::from_ref(&stream))?;
                }
            }
            unsafe {
                fv += 1;
                d3d11_ctx4.Signal(&sync_fence, fv)?;
                sync_fence.SetEventOnCompletion(fv, ev)?;
                WaitForSingleObject(ev, INFINITE);
            }
            let ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;
            println!(
                "  4K D3D11 VideoProcessor RGBA->NV12: {:.3} ms/frame ({:.1} FPS)",
                ms,
                1000.0 / ms
            );
            // Readback 1 pixel to verify BT.709 color
            let mut stg_desc = nv12_desc;
            stg_desc.Usage = D3D11_USAGE_STAGING;
            stg_desc.BindFlags = 0;
            stg_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut stg: Option<ID3D11Texture2D> = None;
            unsafe {
                d3d11_dev.CreateTexture2D(&stg_desc, None, Some(&mut stg))?;
                let stg = stg.unwrap();
                d3d11_ctx.CopyResource(&stg, &nv12_tex);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                d3d11_ctx.Map(&stg, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let y = *(mapped.pData as *const u8);
                let uv_base = (mapped.pData as *const u8).add((mapped.RowPitch * height) as usize);
                let u = *uv_base;
                let v = *uv_base.add(1);
                d3d11_ctx.Unmap(&stg, 0);
                println!("    VP Output pixel (R=64,G=192,B=128) -> Y={y}, U={u}, V={v} (expected BT.709 limited: Y~142, U~119, V~76)");
                let _ = CloseHandle(ev);
                std::mem::ManuallyDrop::into_inner(stream.pInputSurface);
            }
        }

        // Benchmark Compute Shader 4K (if NV12 direct UAV works)
        if nv12_has_uav {
            let mut blob = None;
            let mut err_blob = None;
            let compile_res = unsafe {
                D3DCompile(
                    HLSL_RGBA_TO_NV12.as_ptr() as *const c_void,
                    HLSL_RGBA_TO_NV12.len(),
                    PCSTR::null(),
                    None,
                    None,
                    PCSTR(b"main\0".as_ptr()),
                    PCSTR(b"cs_5_0\0".as_ptr()),
                    0,
                    0,
                    &mut blob,
                    Some(&mut err_blob),
                )
            };
            if let Err(e) = compile_res {
                if let Some(eb) = err_blob {
                    let msg = unsafe {
                        std::slice::from_raw_parts(eb.GetBufferPointer() as *const u8, eb.GetBufferSize())
                    };
                    eprintln!("HLSL compile error: {}", String::from_utf8_lossy(msg));
                }
                return Err(e.into());
            }
            let blob = blob.unwrap();
            let bytecode = unsafe {
                std::slice::from_raw_parts(
                    blob.GetBufferPointer() as *const u8,
                    blob.GetBufferSize(),
                )
            };
            let mut cs: Option<ID3D11ComputeShader> = None;
            unsafe { d3d11_dev.CreateComputeShader(bytecode, None, Some(&mut cs))? };
            let cs = cs.unwrap();

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
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            unsafe {
                d3d11_dev.CreateShaderResourceView(&rgba_tex, Some(&srv_desc), Some(&mut srv))?;
            }
            let srv = srv.unwrap();

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
            let mut uav_y: Option<ID3D11UnorderedAccessView> = None;
            let mut uav_uv: Option<ID3D11UnorderedAccessView> = None;
            unsafe {
                d3d11_dev.CreateUnorderedAccessView(&nv12_tex, Some(&y_uav_desc), Some(&mut uav_y))?;
                d3d11_dev.CreateUnorderedAccessView(
                    &nv12_tex,
                    Some(&uv_uav_desc),
                    Some(&mut uav_uv),
                )?;
            }

            let cb_data: [u32; 4] = [width, height, 0, 0];
            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let cb_sub = D3D11_SUBRESOURCE_DATA {
                pSysMem: cb_data.as_ptr() as *const c_void,
                ..Default::default()
            };
            let mut cb: Option<ID3D11Buffer> = None;
            unsafe { d3d11_dev.CreateBuffer(&cb_desc, Some(&cb_sub), Some(&mut cb))? };

            let mut sync_fence: Option<ID3D11Fence> = None;
            unsafe { d3d11_dev5.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut sync_fence)? };
            let sync_fence = sync_fence.unwrap();
            let ev = unsafe { CreateEventW(None, false, false, None)? };
            let mut fv = 0u64;
            let gx = (width / 2).div_ceil(16);
            let gy = (height / 2).div_ceil(16);
            unsafe {
                d3d11_ctx.CSSetShader(&cs, None);
                d3d11_ctx.CSSetConstantBuffers(0, Some(&[cb.clone()]));
                d3d11_ctx.CSSetShaderResources(0, Some(&[Some(srv.clone())]));
                let uavs = [uav_y.clone(), uav_uv.clone()];
                d3d11_ctx.CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
            }
            for _ in 0..5 {
                unsafe {
                    d3d11_ctx.Dispatch(gx, gy, 1);
                    fv += 1;
                    d3d11_ctx4.Signal(&sync_fence, fv)?;
                    sync_fence.SetEventOnCompletion(fv, ev)?;
                    WaitForSingleObject(ev, INFINITE);
                }
            }
            let iters = 100;
            let t0 = Instant::now();
            for _ in 0..iters {
                unsafe { d3d11_ctx.Dispatch(gx, gy, 1) };
            }
            unsafe {
                fv += 1;
                d3d11_ctx4.Signal(&sync_fence, fv)?;
                sync_fence.SetEventOnCompletion(fv, ev)?;
                WaitForSingleObject(ev, INFINITE);
            }
            let ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;
            println!(
                "  4K D3D11 ComputeShader (direct NV12 UAV) RGBA->NV12: {:.3} ms/frame ({:.1} FPS)",
                ms,
                1000.0 / ms
            );
            // Readback 1 pixel
            let mut stg_desc = nv12_desc;
            stg_desc.Usage = D3D11_USAGE_STAGING;
            stg_desc.BindFlags = 0;
            stg_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut stg: Option<ID3D11Texture2D> = None;
            unsafe {
                d3d11_dev.CreateTexture2D(&stg_desc, None, Some(&mut stg))?;
                let stg = stg.unwrap();
                d3d11_ctx.CopyResource(&stg, &nv12_tex);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                d3d11_ctx.Map(&stg, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let y = *(mapped.pData as *const u8);
                let uv_base = (mapped.pData as *const u8).add((mapped.RowPitch * height) as usize);
                let u = *uv_base;
                let v = *uv_base.add(1);
                d3d11_ctx.Unmap(&stg, 0);
                println!("    CS Output pixel (R=64,G=192,B=128) -> Y={y}, U={u}, V={v}");
                let _ = CloseHandle(ev);
            }
        } else {
            println!("  (Direct NV12 UAV unsupported on this GPU; Compute Shader requires intermediate textures)");
        }

        println!("\n=== 5. Direct Intel QSV (oneVPL/MFX) + D3D11 Zero-Copy Encode Probe ===");
        let mfx = MfxLib::load("libvpl.dll")
            .or_else(|_| MfxLib::load("libmfxhw64.dll"))
            .context("load libvpl.dll or libmfxhw64.dll")?;

        for (label, codec_id, low_power) in [
            ("H.264 VME (LowPower=OFF)", MFX_CODEC_AVC, MFX_CODINGOPTION_OFF),
            ("H.264 VDENC (LowPower=ON)", MFX_CODEC_AVC, MFX_CODINGOPTION_ON),
            ("HEVC VME (LowPower=OFF)", MFX_CODEC_HEVC, MFX_CODINGOPTION_OFF),
            ("HEVC VDENC (LowPower=ON)", MFX_CODEC_HEVC, MFX_CODINGOPTION_ON),
        ] {
            match test_qsv_encode(&mfx, &d3d11_dev, width, height, 60, codec_id, low_power, 4, 60) {
                Ok((fps_val, total_bytes, impl_ver)) => {
                    println!(
                        "  4K60 {:<28}: OK! {:.1} FPS (60 frames -> {} bytes, MFX v{}.{})",
                        label, fps_val, total_bytes, impl_ver.major, impl_ver.minor
                    );
                }
                Err(e) => {
                    println!("  4K60 {:<28}: FAIL ({e:#})", label);
                }
            }
        }

        Ok(())
    }

    fn test_qsv_encode(
        mfx: &MfxLib,
        d3d11_dev: &ID3D11Device,
        width: u32,
        height: u32,
        fps: u32,
        codec_id: u32,
        low_power: u16,
        async_depth: u16,
        num_frames: usize,
    ) -> Result<(f64, usize, MfxVersion)> {
        let mut init_param: MfxInitParam = unsafe { std::mem::zeroed() };
        init_param.implementation = MFX_IMPL_HARDWARE_ANY | MFX_IMPL_VIA_D3D11;
        init_param.version = MfxVersion { major: 1, minor: 0 };
        let mut session: MfxSession = null_mut();
        let st = unsafe { (mfx.init_ex)(init_param, &mut session) };
        if st < 0 {
            bail!("MFXInitEx failed: {st}");
        }
        let mut ver = MfxVersion::default();
        let mut imp = 0u32;
        let mut plat = MfxPlatform::default();
        unsafe {
            (mfx.query_version)(session, &mut ver);
            (mfx.query_impl)(session, &mut imp);
            let _ = (mfx.query_platform)(session, &mut plat);
        }

        let st = unsafe { (mfx.set_handle)(session, MFX_HANDLE_D3D11_DEVICE, d3d11_dev.as_raw()) };
        if st < 0 {
            unsafe { (mfx.close)(session) };
            bail!("MFXVideoCORE_SetHandle(D3D11_DEVICE) failed: {st}");
        }

        let mut alloc_state = Box::new(D3d11AllocState {
            device: d3d11_dev.clone(),
            allocs: Vec::new(),
        });
        let mut allocator = MfxFrameAllocator {
            reserved: [0; 4],
            pthis: &mut *alloc_state as *mut D3d11AllocState as *mut c_void,
            alloc: Some(alloc_cb),
            lock: Some(lock_cb),
            unlock: Some(unlock_cb),
            get_hdl: Some(get_hdl_cb),
            free: Some(free_cb),
        };
        let st = unsafe { (mfx.set_allocator)(session, &mut allocator) };
        if st < 0 {
            unsafe { (mfx.close)(session) };
            bail!("MFXVideoCORE_SetFrameAllocator failed: {st}");
        }

        let aligned_w = width.next_multiple_of(16) as u16;
        let aligned_h = height.next_multiple_of(16) as u16;
        let mut param: MfxVideoParam = unsafe { std::mem::zeroed() };
        param.async_depth = async_depth;
        param.io_pattern = MFX_IOPATTERN_IN_VIDEO_MEMORY;
        param.mfx.codec_id = codec_id;
        param.mfx.target_usage = 4; // balanced
        param.mfx.low_power = low_power;
        param.mfx.rate_control_method = MFX_RATECONTROL_VBR;
        param.mfx.target_kbps = 8000;
        param.mfx.max_kbps = 12000;
        param.mfx.gop_pic_size = 60;
        param.mfx.gop_ref_dist = 1;
        param.mfx.frame_info.fourcc = MFX_FOURCC_NV12;
        param.mfx.frame_info.chroma_format = MFX_CHROMAFORMAT_YUV420;
        param.mfx.frame_info.pic_struct = MFX_PICSTRUCT_PROGRESSIVE;
        param.mfx.frame_info.width = aligned_w;
        param.mfx.frame_info.height = aligned_h;
        param.mfx.frame_info.crop_w = width as u16;
        param.mfx.frame_info.crop_h = height as u16;
        param.mfx.frame_info.frame_rate_ext_n = fps;
        param.mfx.frame_info.frame_rate_ext_d = 1;

        let mut req: MfxFrameAllocRequest = unsafe { std::mem::zeroed() };
        let st = unsafe { (mfx.enc_query_iosurf)(session, &mut param, &mut req) };
        if st < 0 {
            unsafe { (mfx.close)(session) };
            bail!("MFXVideoENCODE_QueryIOSurf failed: {st}");
        }
        req.mem_type |= MFX_MEMTYPE_EXTERNAL_FRAME | MFX_MEMTYPE_FROM_ENCODE;
        req.num_frame_suggested = req.num_frame_suggested.max(async_depth + 4);
        let mut resp: MfxFrameAllocResponse = unsafe { std::mem::zeroed() };
        let st = unsafe { alloc_cb(allocator.pthis, &mut req, &mut resp) };
        if st < 0 {
            unsafe { (mfx.close)(session) };
            bail!("external surface alloc failed: {st}");
        }

        let st = unsafe { (mfx.enc_init)(session, &mut param) };
        if st < 0 {
            unsafe { (mfx.close)(session) };
            bail!("MFXVideoENCODE_Init failed: {st}");
        }

        let n_surf = resp.num_frame_actual as usize;
        let mut surfaces: Vec<MfxFrameSurface1> = (0..n_surf)
            .map(|i| {
                let mut s: MfxFrameSurface1 = unsafe { std::mem::zeroed() };
                s.info = param.mfx.frame_info;
                s.data.mem_id = unsafe { *resp.mids.add(i) };
                s
            })
            .collect();

        // Prepare async bitstream ring
        let depth = async_depth as usize;
        let mut bs_bufs: Vec<Vec<u8>> = (0..depth).map(|_| vec![0u8; 4 * 1024 * 1024]).collect();
        let mut bitstreams: Vec<MfxBitstream> = bs_bufs
            .iter_mut()
            .map(|buf| {
                let mut bs: MfxBitstream = unsafe { std::mem::zeroed() };
                bs.data = buf.as_mut_ptr();
                bs.max_length = buf.len() as u32;
                bs
            })
            .collect();
        let mut syncs: Vec<MfxSyncPoint> = vec![null_mut(); depth];

        let mut total_bytes = 0usize;
        let t0 = Instant::now();
        let mut pending: std::collections::VecDeque<usize> = std::collections::VecDeque::new();

        for f in 0..num_frames {
            if pending.len() >= depth {
                let slot = pending.pop_front().unwrap();
                let st = unsafe { (mfx.sync_op)(session, syncs[slot], 60_000) };
                if st < 0 {
                    unsafe {
                        (mfx.enc_close)(session);
                        (mfx.close)(session);
                    }
                    bail!("SyncOperation failed: {st}");
                }
                total_bytes += bitstreams[slot].data_length as usize;
                bitstreams[slot].data_length = 0;
                bitstreams[slot].data_offset = 0;
                syncs[slot] = null_mut();
            }

            let surf_idx = loop {
                if let Some(i) = surfaces.iter().position(|s| s.data.locked == 0) {
                    break i;
                }
                std::thread::yield_now();
            };
            let bs_slot = f % depth;
            surfaces[surf_idx].data.timestamp = (f as u64) * 90_000 / (fps as u64);
            loop {
                let st = unsafe {
                    (mfx.enc_frame_async)(
                        session,
                        null_mut(),
                        &mut surfaces[surf_idx],
                        &mut bitstreams[bs_slot],
                        &mut syncs[bs_slot],
                    )
                };
                if st == MFX_WRN_DEVICE_BUSY {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                    continue;
                }
                if st < 0 && st != MFX_ERR_MORE_DATA && st != MFX_ERR_MORE_SURFACE {
                    unsafe {
                        (mfx.enc_close)(session);
                        (mfx.close)(session);
                    }
                    bail!("EncodeFrameAsync failed at frame {f}: {st}");
                }
                if !syncs[bs_slot].is_null() {
                    pending.push_back(bs_slot);
                }
                break;
            }
        }
        // Drain pending + flush
        while let Some(slot) = pending.pop_front() {
            let st = unsafe { (mfx.sync_op)(session, syncs[slot], 60_000) };
            if st >= 0 {
                total_bytes += bitstreams[slot].data_length as usize;
            }
            bitstreams[slot].data_length = 0;
            bitstreams[slot].data_offset = 0;
            syncs[slot] = null_mut();
        }
        loop {
            let mut sp: MfxSyncPoint = null_mut();
            let st = unsafe {
                (mfx.enc_frame_async)(session, null_mut(), null_mut(), &mut bitstreams[0], &mut sp)
            };
            if st == MFX_ERR_MORE_DATA {
                break;
            }
            if st >= 0 && !sp.is_null() {
                unsafe { (mfx.sync_op)(session, sp, 60_000) };
                total_bytes += bitstreams[0].data_length as usize;
                bitstreams[0].data_length = 0;
                bitstreams[0].data_offset = 0;
            } else if st == MFX_WRN_IN_EXECUTION || st == MFX_WRN_DEVICE_BUSY {
                std::thread::sleep(std::time::Duration::from_micros(200));
            } else {
                break;
            }
        }
        let elapsed = t0.elapsed().as_secs_f64();
        unsafe {
            (mfx.enc_close)(session);
            (mfx.close)(session);
        }
        Ok((num_frames as f64 / elapsed, total_bytes, ver))
    }
}

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
    win_probe::run()
}
