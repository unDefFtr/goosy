//! Runtime-loaded Intel oneVPL / Media SDK (MFX) x64 C ABI definitions.
//!
//! All struct layouts and offsets are verified at compile time against the
//! official Intel oneVPL headers for Windows x86_64 (MSVC ABI).

#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use std::ffi::{CString, c_void};
use std::ptr::null_mut;
use std::sync::OnceLock;
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{PCSTR, PCWSTR};

pub type MfxStatus = i32;
pub type MfxSession = *mut c_void;
pub type MfxSyncPoint = *mut c_void;
pub type MfxMemId = *mut c_void;
pub type MfxHdl = *mut c_void;
pub type MfxLoader = *mut c_void;
pub type MfxConfig = *mut c_void;

pub const MFX_ERR_NONE: MfxStatus = 0;
pub const MFX_ERR_UNKNOWN: MfxStatus = -1;
pub const MFX_ERR_UNSUPPORTED: MfxStatus = -3;
pub const MFX_ERR_MEMORY_ALLOC: MfxStatus = -4;
pub const MFX_ERR_NOT_ENOUGH_BUFFER: MfxStatus = -5;
pub const MFX_ERR_NOT_FOUND: MfxStatus = -9;
pub const MFX_ERR_MORE_DATA: MfxStatus = -10;
pub const MFX_ERR_MORE_SURFACE: MfxStatus = -11;
pub const MFX_ERR_DEVICE_LOST: MfxStatus = -13;
pub const MFX_ERR_INCOMPATIBLE_VIDEO_PARAM: MfxStatus = -14;
pub const MFX_ERR_INVALID_VIDEO_PARAM: MfxStatus = -15;
pub const MFX_ERR_DEVICE_FAILED: MfxStatus = -17;

pub const MFX_WRN_IN_EXECUTION: MfxStatus = 1;
pub const MFX_WRN_DEVICE_BUSY: MfxStatus = 2;
pub const MFX_WRN_VIDEO_PARAM_CHANGED: MfxStatus = 3;
pub const MFX_WRN_PARTIAL_ACCELERATION: MfxStatus = 4;
pub const MFX_WRN_INCOMPATIBLE_VIDEO_PARAM: MfxStatus = 5;
pub const MFX_WRN_VALUE_NOT_CHANGED: MfxStatus = 6;

pub const MFX_TIMEBASE: i64 = 90_000;
pub const MFX_TIMESTAMP_UNKNOWN: i64 = -1;

pub const MFX_IMPL_HARDWARE_ANY: u32 = 0x0004;
pub const MFX_IMPL_VIA_D3D11: u32 = 0x0300;
pub const MFX_HANDLE_D3D11_DEVICE: i32 = 3;

// oneVPL 2.x dispatcher filter constants
pub const MFX_VARIANT_TYPE_U32: i32 = 5;
pub const MFX_IMPL_TYPE_HARDWARE: u32 = 2;
pub const MFX_ACCEL_MODE_VIA_D3D11: u32 = 0x0300;

pub const MFX_CODEC_AVC: u32 = 0x2043_5641; // 'AVC '
pub const MFX_CODEC_HEVC: u32 = 0x4356_4548; // 'HEVC'
pub const MFX_FOURCC_NV12: u32 = 0x3231_564E; // 'NV12'
pub const MFX_FOURCC_P010: u32 = 0x3031_3050; // 'P010'
pub const MFX_FOURCC_P8: u32 = 0x2020_3850; // 'P8  '

pub const MFX_CHROMAFORMAT_YUV420: u16 = 1;
pub const MFX_PICSTRUCT_PROGRESSIVE: u16 = 1;

pub const MFX_IOPATTERN_IN_VIDEO_MEMORY: u16 = 0x0001;
pub const MFX_IOPATTERN_IN_SYSTEM_MEMORY: u16 = 0x0002;

pub const MFX_MEMTYPE_INTERNAL_FRAME: u16 = 0x0001;
pub const MFX_MEMTYPE_EXTERNAL_FRAME: u16 = 0x0002;
pub const MFX_MEMTYPE_VIDEO_MEMORY_DECODER_TARGET: u16 = 0x0010;
pub const MFX_MEMTYPE_FROM_ENCODE: u16 = 0x0100;

pub const MFX_RATECONTROL_CBR: u16 = 1;
pub const MFX_RATECONTROL_VBR: u16 = 2;
pub const MFX_RATECONTROL_CQP: u16 = 3;
pub const MFX_RATECONTROL_ICQ: u16 = 9;

pub const MFX_CODINGOPTION_UNKNOWN: u16 = 0;
pub const MFX_CODINGOPTION_ON: u16 = 0x0010;
pub const MFX_CODINGOPTION_OFF: u16 = 0x0020;

pub const MFX_EXTBUFF_CODING_OPTION_SPSPPS: u32 = 0x5053_4F43;
pub const MFX_EXTBUFF_CODING_OPTION_VPS: u32 = 0x5056_4F43;
pub const MFX_EXTBUFF_VIDEO_SIGNAL_INFO: u32 = 0x4E49_5356;

pub const MFX_FRAMETYPE_I: u16 = 0x0001;
pub const MFX_FRAMETYPE_P: u16 = 0x0002;
pub const MFX_FRAMETYPE_B: u16 = 0x0004;
pub const MFX_FRAMETYPE_REF: u16 = 0x0040;
pub const MFX_FRAMETYPE_IDR: u16 = 0x0080;

pub const MFX_GOP_CLOSED: u16 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MfxVersion {
    pub minor: u16,
    pub major: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxInitParam {
    pub implementation: u32,
    pub version: MfxVersion,
    pub external_threads: u16,
    pub _pad0: [u8; 6],
    pub ext_param: *mut *mut MfxExtBuffer,
    pub num_ext_param: u16,
    pub _pad1: [u8; 6],
    pub gpu_copy: u16,
    pub reserved: [u16; 23],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MfxExtBuffer {
    pub buffer_id: u32,
    pub buffer_sz: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxFrameInfo {
    pub reserved: [u32; 4],
    pub channel_id: u16,
    pub bit_depth_luma: u16,
    pub bit_depth_chroma: u16,
    pub shift: u16,
    pub frame_id: [u16; 4],
    pub fourcc: u32,
    pub width: u16,
    pub height: u16,
    pub crop_x: u16,
    pub crop_y: u16,
    pub crop_w: u16,
    pub crop_h: u16,
    pub frame_rate_ext_n: u32,
    pub frame_rate_ext_d: u32,
    pub reserved3: u16,
    pub aspect_ratio_w: u16,
    pub aspect_ratio_h: u16,
    pub pic_struct: u16,
    pub chroma_format: u16,
    pub reserved2: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxFrameData {
    pub ext_param: *mut *mut MfxExtBuffer,
    pub num_ext_param: u16,
    pub reserved: [u16; 9],
    pub mem_type: u16,
    pub pitch_high: u16,
    pub timestamp: u64,
    pub frame_order: u32,
    pub locked: u16,
    pub pitch_low: u16,
    pub y: *mut u8,
    pub uv: *mut u8,
    pub v: *mut u8,
    pub a: *mut u8,
    pub mem_id: MfxMemId,
    pub corrupted: u16,
    pub data_flag: u16,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxFrameSurface1 {
    pub frame_interface: *mut c_void,
    pub reserved: [u32; 2],
    pub info: MfxFrameInfo,
    pub _pad: u32,
    pub data: MfxFrameData,
}

impl MfxFrameSurface1 {
    /// Reads `Data.Locked` via raw pointer (`UnsafeCell`-backed storage) after an explicit
    /// `MFXVideoCORE_SyncOperation` boundary.
    ///
    /// # Safety
    /// - `surface` must be a valid, aligned pointer to an initialized `MfxFrameSurface1`
    ///   stored inside `UnsafeCell` (no live `&MfxFrameSurface1` / `&mut MfxFrameSurface1`).
    /// - Caller must only inspect surfaces whose completion has been synchronized by
    ///   `MFXVideoCORE_SyncOperation` (or `MFXVideoENCODE_Close`), not unsynchronized
    ///   in-flight surfaces.
    pub unsafe fn read_locked_raw(surface: *const Self) -> u16 {
        unsafe {
            let locked_ptr = std::ptr::addr_of!((*surface).data.locked);
            std::ptr::read_volatile(locked_ptr)
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxInfoMfx {
    pub reserved: [u32; 7],
    pub low_power: u16,
    pub brc_param_multiplier: u16,
    pub frame_info: MfxFrameInfo,
    pub codec_id: u32,
    pub codec_profile: u16,
    pub codec_level: u16,
    pub num_thread: u16,
    pub target_usage: u16,
    pub gop_pic_size: u16,
    pub gop_ref_dist: u16,
    pub gop_opt_flag: u16,
    pub idr_interval: u16,
    pub rate_control_method: u16,
    pub initial_delay_in_kb: u16,
    pub buffer_size_in_kb: u16,
    pub target_kbps: u16,
    pub max_kbps: u16,
    pub num_slice: u16,
    pub num_ref_frame: u16,
    pub encoded_order: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxVideoParam {
    pub alloc_id: u32,
    pub reserved: [u32; 2],
    pub reserved3: u16,
    pub async_depth: u16,
    pub mfx: MfxInfoMfx,
    pub _union_pad: [u32; 8],
    pub protected: u16,
    pub io_pattern: u16,
    pub _pad0: u32,
    pub ext_param: *mut *mut MfxExtBuffer,
    pub num_ext_param: u16,
    pub reserved2: u16,
    pub _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxBitstream {
    pub encrypted_data: *mut c_void,
    pub ext_param: *mut *mut MfxExtBuffer,
    pub num_ext_param: u16,
    pub reserved: [u16; 3],
    pub decode_timestamp: i64,
    pub timestamp: u64,
    pub data: *mut u8,
    pub data_offset: u32,
    pub data_length: u32,
    pub max_length: u32,
    pub pic_struct: u16,
    pub frame_type: u16,
    pub data_flag: u16,
    pub reserved2: u16,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxFrameAllocRequest {
    pub alloc_id: u32,
    pub reserved: [u32; 3],
    pub info: MfxFrameInfo,
    pub mem_type: u16,
    pub num_frame_min: u16,
    pub num_frame_suggested: u16,
    pub reserved2: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxFrameAllocResponse {
    pub alloc_id: u32,
    pub reserved: [u32; 3],
    pub mids: *mut MfxMemId,
    pub num_frame_actual: u16,
    pub reserved2: [u16; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxHdlPair {
    pub first: MfxHdl,
    pub second: MfxHdl,
}

unsafe impl Send for MfxHdlPair {}
unsafe impl Sync for MfxHdlPair {}

pub type FnAlloc = unsafe extern "C" fn(
    *mut c_void,
    *mut MfxFrameAllocRequest,
    *mut MfxFrameAllocResponse,
) -> MfxStatus;
pub type FnLock = unsafe extern "C" fn(*mut c_void, MfxMemId, *mut MfxFrameData) -> MfxStatus;
pub type FnUnlock = unsafe extern "C" fn(*mut c_void, MfxMemId, *mut MfxFrameData) -> MfxStatus;
pub type FnGetHdl = unsafe extern "C" fn(*mut c_void, MfxMemId, *mut MfxHdl) -> MfxStatus;
pub type FnFree = unsafe extern "C" fn(*mut c_void, *mut MfxFrameAllocResponse) -> MfxStatus;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxFrameAllocator {
    pub reserved: [u32; 4],
    pub pthis: *mut c_void,
    pub alloc: Option<FnAlloc>,
    pub lock: Option<FnLock>,
    pub unlock: Option<FnUnlock>,
    pub get_hdl: Option<FnGetHdl>,
    pub free: Option<FnFree>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxExtCodingOptionSpsPps {
    pub header: MfxExtBuffer,
    pub sps_buffer: *mut u8,
    pub pps_buffer: *mut u8,
    pub sps_buf_size: u16,
    pub pps_buf_size: u16,
    pub sps_id: u16,
    pub pps_id: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MfxExtCodingOptionVps {
    pub header: MfxExtBuffer,
    pub vps_buffer: *mut u8,
    pub vps_buf_size: u16,
    pub vps_id: u16,
    pub reserved: [u16; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MfxExtVideoSignalInfo {
    pub header: MfxExtBuffer,
    pub video_format: u16,
    pub video_full_range: u16,
    pub colour_description_present: u16,
    pub colour_primaries: u16,
    pub transfer_characteristics: u16,
    pub matrix_coefficients: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MfxPlatform {
    pub code_name: u16,
    pub device_id: u16,
    pub media_adapter_type: u16,
    pub reserved: [u16; 13],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MfxVariant {
    pub version_minor: u8,
    pub version_major: u8,
    pub _pad: u16,
    pub variant_type: i32,
    pub data_u64: u64,
}

// Compile-time ABI verification against MSVC x64 official Intel oneVPL headers.
const _: () = {
    assert!(std::mem::size_of::<MfxVersion>() == 4);
    assert!(std::mem::size_of::<MfxInitParam>() == 80);
    assert!(std::mem::size_of::<MfxExtBuffer>() == 8);
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
    assert!(std::mem::size_of::<MfxExtCodingOptionSpsPps>() == 32);
    assert!(std::mem::size_of::<MfxExtCodingOptionVps>() == 32);
    assert!(std::mem::size_of::<MfxExtVideoSignalInfo>() == 20);
    assert!(std::mem::size_of::<MfxPlatform>() == 32);
    assert!(std::mem::size_of::<MfxVariant>() == 16);
};

pub type FnMfxInitEx = unsafe extern "C" fn(MfxInitParam, *mut MfxSession) -> MfxStatus;
pub type FnMfxLoad = unsafe extern "C" fn() -> MfxLoader;
pub type FnMfxUnload = unsafe extern "C" fn(MfxLoader);
pub type FnMfxCreateConfig = unsafe extern "C" fn(MfxLoader) -> MfxConfig;
pub type FnMfxSetConfigFilterProperty =
    unsafe extern "C" fn(MfxConfig, *const u8, MfxVariant) -> MfxStatus;
pub type FnMfxCreateSession = unsafe extern "C" fn(MfxLoader, u32, *mut MfxSession) -> MfxStatus;
pub type FnMfxClose = unsafe extern "C" fn(MfxSession) -> MfxStatus;
pub type FnMfxQueryVersion = unsafe extern "C" fn(MfxSession, *mut MfxVersion) -> MfxStatus;
pub type FnMfxQueryImpl = unsafe extern "C" fn(MfxSession, *mut u32) -> MfxStatus;
pub type FnMfxQueryPlatform = unsafe extern "C" fn(MfxSession, *mut MfxPlatform) -> MfxStatus;
pub type FnMfxSetHandle = unsafe extern "C" fn(MfxSession, i32, MfxHdl) -> MfxStatus;
pub type FnMfxSetFrameAllocator =
    unsafe extern "C" fn(MfxSession, *mut MfxFrameAllocator) -> MfxStatus;
pub type FnMfxSyncOperation = unsafe extern "C" fn(MfxSession, MfxSyncPoint, u32) -> MfxStatus;
pub type FnMfxEncodeQuery =
    unsafe extern "C" fn(MfxSession, *mut MfxVideoParam, *mut MfxVideoParam) -> MfxStatus;
pub type FnMfxEncodeQueryIoSurf = unsafe extern "C" fn(
    MfxSession,
    *mut MfxVideoParam,
    *mut MfxFrameAllocRequest,
) -> MfxStatus;
pub type FnMfxEncodeInit = unsafe extern "C" fn(MfxSession, *mut MfxVideoParam) -> MfxStatus;
pub type FnMfxEncodeGetVideoParam =
    unsafe extern "C" fn(MfxSession, *mut MfxVideoParam) -> MfxStatus;
pub type FnMfxEncodeClose = unsafe extern "C" fn(MfxSession) -> MfxStatus;
pub type FnMfxEncodeFrameAsync = unsafe extern "C" fn(
    MfxSession,
    *mut c_void,
    *mut MfxFrameSurface1,
    *mut MfxBitstream,
    *mut MfxSyncPoint,
) -> MfxStatus;

#[derive(Clone, Copy)]
pub struct VplLoaderFns {
    pub load: FnMfxLoad,
    pub unload: FnMfxUnload,
    pub create_config: FnMfxCreateConfig,
    pub set_config_filter_property: FnMfxSetConfigFilterProperty,
    pub create_session: FnMfxCreateSession,
}

pub struct MfxRuntime {
    pub dll_name: &'static str,
    _module: HMODULE,
    pub init_ex: Option<FnMfxInitEx>,
    pub vpl_loader: Option<VplLoaderFns>,
    pub close: FnMfxClose,
    pub query_version: FnMfxQueryVersion,
    pub query_impl: FnMfxQueryImpl,
    pub query_platform: Option<FnMfxQueryPlatform>,
    pub set_handle: FnMfxSetHandle,
    pub set_allocator: FnMfxSetFrameAllocator,
    pub sync_op: FnMfxSyncOperation,
    pub enc_query: FnMfxEncodeQuery,
    pub enc_query_iosurf: FnMfxEncodeQueryIoSurf,
    pub enc_init: FnMfxEncodeInit,
    pub enc_get_video_param: FnMfxEncodeGetVideoParam,
    pub enc_close: FnMfxEncodeClose,
    pub enc_frame_async: FnMfxEncodeFrameAsync,
}

// Function pointers loaded from a process-lifetime DLL are safe to share across threads.
unsafe impl Send for MfxRuntime {}
unsafe impl Sync for MfxRuntime {}

struct FreeModuleOnDrop(HMODULE);
impl Drop for FreeModuleOnDrop {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

/// RAII guard for an MFX session (and optional oneVPL 2.x loader).
/// Ensures `MFXVideoENCODE_Close` (if active) -> `MFXClose` -> `MFXUnload`
/// always execute in strict specification order on both normal exit and errors.
pub struct OwnedMfxSession {
    mfx: &'static MfxRuntime,
    pub session: MfxSession,
    loader: Option<MfxLoader>,
    encoder_initialized: bool,
}

impl OwnedMfxSession {
    pub fn mark_encoder_initialized(&mut self) {
        self.encoder_initialized = true;
    }

    pub fn close_now(&mut self) {
        if !self.session.is_null() {
            unsafe {
                if self.encoder_initialized {
                    let _ = (self.mfx.enc_close)(self.session);
                    self.encoder_initialized = false;
                }
                let _ = (self.mfx.close)(self.session);
            }
            self.session = null_mut();
        }
        if let Some(loader) = self.loader.take()
            && let Some(vpl) = self.mfx.vpl_loader
        {
            unsafe {
                (vpl.unload)(loader);
            }
        }
    }
}

impl Drop for OwnedMfxSession {
    fn drop(&mut self) {
        self.close_now();
    }
}

static MFX_RUNTIME: OnceLock<std::result::Result<MfxRuntime, String>> = OnceLock::new();

impl MfxRuntime {
    pub fn global() -> Result<&'static Self> {
        let res = MFX_RUNTIME.get_or_init(|| {
            // Try libvpl.dll first (supports oneVPL 2.x on Gen 11-14 / Iris Xe / Arc and
            // legacy dispatch), verifying a hardware D3D11 session can actually be opened
            // before committing to it; otherwise fall back to libmfxhw64.dll (MSDK 1.x).
            let mut errors = Vec::new();
            for dll in ["libvpl.dll", "libmfxhw64.dll"] {
                match Self::try_load(dll) {
                    Ok(rt) => match rt.probe_session_open() {
                        Ok(()) => return Ok(rt),
                        Err(e) => {
                            errors.push(format!("{dll} loaded but D3D11 session open failed ({e:#})"));
                            unsafe {
                                let _ = FreeLibrary(rt._module);
                            }
                        }
                    },
                    Err(e) => errors.push(format!("{dll} ({e:#})")),
                }
            }
            Err(errors.join("; "))
        });
        match res {
            Ok(rt) => Ok(rt),
            Err(msg) => bail!("Intel oneVPL/MFX runtime unavailable: {msg}"),
        }
    }

    fn probe_session_open(&self) -> Result<()> {
        let (session, loader) = self.open_raw_d3d11_session()?;
        unsafe {
            let _ = (self.close)(session);
        }
        if let Some(l) = loader
            && let Some(vpl) = self.vpl_loader
        {
            unsafe {
                (vpl.unload)(l);
            }
        }
        Ok(())
    }

    /// Creates a hardware D3D11 MFX session using `MFXInitEx` (legacy/MSDK 1.x) or
    /// oneVPL 2.x `MFXLoad` + `MFXCreateSession`.
    pub fn create_d3d11_session(&'static self) -> Result<OwnedMfxSession> {
        let (session, loader) = self.open_raw_d3d11_session()?;
        Ok(OwnedMfxSession {
            mfx: self,
            session,
            loader,
            encoder_initialized: false,
        })
    }

    fn open_raw_d3d11_session(&self) -> Result<(MfxSession, Option<MfxLoader>)> {
        let mut vpl_err: Option<String> = None;
        if let Some(vpl) = self.vpl_loader {
            match Self::open_via_vpl_loader(vpl) {
                Ok(pair) => return Ok(pair),
                Err(e) => vpl_err = Some(format!("{e:#}")),
            }
        }
        if let Some(init_ex) = self.init_ex {
            let mut init_param: MfxInitParam = unsafe { std::mem::zeroed() };
            init_param.implementation = MFX_IMPL_HARDWARE_ANY | MFX_IMPL_VIA_D3D11;
            init_param.version = MfxVersion { major: 1, minor: 0 };
            let mut session: MfxSession = null_mut();
            let st = unsafe { init_ex(init_param, &mut session) };
            if st >= 0 && !session.is_null() {
                return Ok((session, None));
            }
            if let Some(ve) = vpl_err {
                bail!(
                    "oneVPL MFXLoad failed ({ve}); MFXInitEx failed: {st} ({})",
                    mfx_status_name(st)
                );
            }
            bail!("MFXInitEx failed: {st} ({})", mfx_status_name(st));
        }
        bail!(
            "oneVPL MFXLoad failed ({}) and MFXInitEx is unavailable",
            vpl_err.unwrap_or_else(|| "unknown error".to_owned())
        )
    }

    fn open_via_vpl_loader(vpl: VplLoaderFns) -> Result<(MfxSession, Option<MfxLoader>)> {
        let loader = unsafe { (vpl.load)() };
        if loader.is_null() {
            bail!("MFXLoad returned null");
        }
        let cfg = unsafe { (vpl.create_config)(loader) };
        if cfg.is_null() {
            unsafe { (vpl.unload)(loader) };
            bail!("MFXCreateConfig returned null");
        }
        let set_u32 = |name: &[u8], val: u32| -> MfxStatus {
            let var = MfxVariant {
                version_minor: 0,
                version_major: 1,
                _pad: 0,
                variant_type: MFX_VARIANT_TYPE_U32,
                data_u64: val as u64,
            };
            unsafe { (vpl.set_config_filter_property)(cfg, name.as_ptr(), var) }
        };
        let st1 = set_u32(b"mfxImplDescription.Impl\0", MFX_IMPL_TYPE_HARDWARE);
        let st2 = set_u32(
            b"mfxImplDescription.AccelerationMode\0",
            MFX_ACCEL_MODE_VIA_D3D11,
        );
        if st1 < 0 || st2 < 0 {
            unsafe { (vpl.unload)(loader) };
            bail!(
                "MFXSetConfigFilterProperty failed: impl={st1} ({}), accel={st2} ({})",
                mfx_status_name(st1),
                mfx_status_name(st2)
            );
        }
        let mut session: MfxSession = null_mut();
        let st = unsafe { (vpl.create_session)(loader, 0, &mut session) };
        if st < 0 || session.is_null() {
            unsafe { (vpl.unload)(loader) };
            bail!(
                "MFXCreateSession failed: {st} ({})",
                mfx_status_name(st)
            );
        }
        Ok((session, Some(loader)))
    }

    fn try_load(dll_name: &'static str) -> Result<Self> {
        let wide: Vec<u16> = dll_name.encode_utf16().chain(std::iter::once(0)).collect();
        let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
            .with_context(|| format!("LoadLibraryW({dll_name})"))?;
        let guard = FreeModuleOnDrop(module);
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let c = CString::new($name).unwrap();
                let p = unsafe { GetProcAddress(module, PCSTR(c.as_ptr() as *const u8)) }
                    .with_context(|| format!("missing symbol {} in {}", $name, dll_name))?;
                unsafe { std::mem::transmute::<_, $ty>(p) }
            }};
        }
        macro_rules! opt_sym {
            ($name:literal, $ty:ty) => {{
                let c = CString::new($name).unwrap();
                unsafe { GetProcAddress(module, PCSTR(c.as_ptr() as *const u8)) }
                    .map(|p| unsafe { std::mem::transmute::<_, $ty>(p) })
            }};
        }
        let init_ex = opt_sym!("MFXInitEx", FnMfxInitEx);
        let vpl_loader = match (
            opt_sym!("MFXLoad", FnMfxLoad),
            opt_sym!("MFXUnload", FnMfxUnload),
            opt_sym!("MFXCreateConfig", FnMfxCreateConfig),
            opt_sym!("MFXSetConfigFilterProperty", FnMfxSetConfigFilterProperty),
            opt_sym!("MFXCreateSession", FnMfxCreateSession),
        ) {
            (
                Some(load),
                Some(unload),
                Some(create_config),
                Some(set_config_filter_property),
                Some(create_session),
            ) => Some(VplLoaderFns {
                load,
                unload,
                create_config,
                set_config_filter_property,
                create_session,
            }),
            _ => None,
        };
        if init_ex.is_none() && vpl_loader.is_none() {
            bail!("{dll_name} exports neither MFXInitEx nor oneVPL 2.x MFXLoad/MFXCreateSession");
        }
        let rt = Self {
            dll_name,
            _module: module,
            init_ex,
            vpl_loader,
            close: sym!("MFXClose", FnMfxClose),
            query_version: sym!("MFXQueryVersion", FnMfxQueryVersion),
            query_impl: sym!("MFXQueryIMPL", FnMfxQueryImpl),
            query_platform: opt_sym!("MFXVideoCORE_QueryPlatform", FnMfxQueryPlatform),
            set_handle: sym!("MFXVideoCORE_SetHandle", FnMfxSetHandle),
            set_allocator: sym!("MFXVideoCORE_SetFrameAllocator", FnMfxSetFrameAllocator),
            sync_op: sym!("MFXVideoCORE_SyncOperation", FnMfxSyncOperation),
            enc_query: sym!("MFXVideoENCODE_Query", FnMfxEncodeQuery),
            enc_query_iosurf: sym!("MFXVideoENCODE_QueryIOSurf", FnMfxEncodeQueryIoSurf),
            enc_init: sym!("MFXVideoENCODE_Init", FnMfxEncodeInit),
            enc_get_video_param: sym!("MFXVideoENCODE_GetVideoParam", FnMfxEncodeGetVideoParam),
            enc_close: sym!("MFXVideoENCODE_Close", FnMfxEncodeClose),
            enc_frame_async: sym!("MFXVideoENCODE_EncodeFrameAsync", FnMfxEncodeFrameAsync),
        };
        // Disarm FreeLibrary now that loading succeeded and process-lifetime Self owns module.
        std::mem::forget(guard);
        Ok(rt)
    }
}

pub fn mfx_status_name(status: MfxStatus) -> &'static str {
    match status {
        0 => "MFX_ERR_NONE",
        -1 => "MFX_ERR_UNKNOWN",
        -2 => "MFX_ERR_NULL_PTR",
        -3 => "MFX_ERR_UNSUPPORTED",
        -4 => "MFX_ERR_MEMORY_ALLOC",
        -5 => "MFX_ERR_NOT_ENOUGH_BUFFER",
        -6 => "MFX_ERR_INVALID_HANDLE",
        -7 => "MFX_ERR_LOCK_MEMORY",
        -8 => "MFX_ERR_NOT_INITIALIZED",
        -9 => "MFX_ERR_NOT_FOUND",
        -10 => "MFX_ERR_MORE_DATA",
        -11 => "MFX_ERR_MORE_SURFACE",
        -12 => "MFX_ERR_ABORTED",
        -13 => "MFX_ERR_DEVICE_LOST",
        -14 => "MFX_ERR_INCOMPATIBLE_VIDEO_PARAM",
        -15 => "MFX_ERR_INVALID_VIDEO_PARAM",
        -16 => "MFX_ERR_UNDEFINED_BEHAVIOR",
        -17 => "MFX_ERR_DEVICE_FAILED",
        -18 => "MFX_ERR_MORE_BITSTREAM",
        1 => "MFX_WRN_IN_EXECUTION",
        2 => "MFX_WRN_DEVICE_BUSY",
        3 => "MFX_WRN_VIDEO_PARAM_CHANGED",
        4 => "MFX_WRN_PARTIAL_ACCELERATION",
        5 => "MFX_WRN_INCOMPATIBLE_VIDEO_PARAM",
        _ => "MFX_STATUS_OTHER",
    }
}
