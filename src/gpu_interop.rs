//! Linux Vulkan/CUDA interop contracts and validation.

#[cfg(target_os = "linux")]
use anyhow::{Context, Result, bail};
#[cfg(target_os = "linux")]
use std::ffi::{CStr, CString, c_char, c_int, c_void};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
#[cfg(target_os = "linux")]
const RTLD_NOW: c_int = 2;
#[cfg(target_os = "linux")]
type CuResult = i32;
#[cfg(target_os = "linux")]
type CuDevice = i32;
#[cfg(target_os = "linux")]
type CuContext = *mut c_void;
#[cfg(target_os = "linux")]
type CuExternalMemory = *mut c_void;
#[cfg(target_os = "linux")]
type CuExternalSemaphore = *mut c_void;
#[cfg(target_os = "linux")]
type CuStream = *mut c_void;

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CudaUuid {
    bytes: [u8; 16],
}
#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CudaWin32Handle {
    handle: *mut c_void,
    name: *const c_void,
}
#[cfg(target_os = "linux")]
#[repr(C)]
union CudaExternalMemoryHandle {
    fd: c_int,
    win32: CudaWin32Handle,
    nv_sci_buf_object: *const c_void,
}
#[cfg(target_os = "linux")]
#[repr(C)]
struct CudaExternalMemoryHandleDesc {
    type_: u32,
    handle: CudaExternalMemoryHandle,
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}
#[cfg(target_os = "linux")]
#[repr(C)]
struct CudaExternalMemoryBufferDesc {
    offset: u64,
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}
#[cfg(target_os = "linux")]
#[repr(C)]
union CudaExternalSemaphoreHandle {
    fd: c_int,
    win32: CudaWin32Handle,
    nv_sci_sync_obj: *const c_void,
}
#[cfg(target_os = "linux")]
#[repr(C)]
struct CudaExternalSemaphoreHandleDesc {
    type_: u32,
    handle: CudaExternalSemaphoreHandle,
    flags: u32,
    reserved: [u32; 16],
}
#[cfg(target_os = "linux")]
#[repr(C)]
struct CudaExternalSemaphoreWaitParams {
    params: [u64; 9],
    flags: u32,
    reserved: [u32; 16],
}
#[cfg(target_os = "linux")]
#[repr(C)]
struct CudaExternalSemaphoreSignalParams {
    params: [u64; 9],
    flags: u32,
    reserved: [u32; 16],
}

#[cfg(target_os = "linux")]
type CuInit = unsafe extern "C" fn(u32) -> CuResult;
#[cfg(target_os = "linux")]
type CuDeviceGet = unsafe extern "C" fn(*mut CuDevice, i32) -> CuResult;
#[cfg(target_os = "linux")]
type CuDeviceGetUuid = unsafe extern "C" fn(*mut CudaUuid, CuDevice) -> CuResult;
#[cfg(target_os = "linux")]
type CuDevicePrimaryCtxRetain = unsafe extern "C" fn(*mut CuContext, CuDevice) -> CuResult;
#[cfg(target_os = "linux")]
type CuDevicePrimaryCtxRelease = unsafe extern "C" fn(CuDevice) -> CuResult;
#[cfg(target_os = "linux")]
type CuCtxSetCurrent = unsafe extern "C" fn(CuContext) -> CuResult;
#[cfg(target_os = "linux")]
type CuGetErrorName = unsafe extern "C" fn(CuResult, *mut *const c_char) -> CuResult;
#[cfg(target_os = "linux")]
type CuImportExternalMemory =
    unsafe extern "C" fn(*mut CuExternalMemory, *const CudaExternalMemoryHandleDesc) -> CuResult;
#[cfg(target_os = "linux")]
type CuMapExternalBuffer = unsafe extern "C" fn(
    *mut u64,
    CuExternalMemory,
    *const CudaExternalMemoryBufferDesc,
) -> CuResult;
#[cfg(target_os = "linux")]
type CuDestroyExternalMemory = unsafe extern "C" fn(CuExternalMemory) -> CuResult;
#[cfg(target_os = "linux")]
type CuImportExternalSemaphore = unsafe extern "C" fn(
    *mut CuExternalSemaphore,
    *const CudaExternalSemaphoreHandleDesc,
) -> CuResult;
#[cfg(target_os = "linux")]
type CuDestroyExternalSemaphore = unsafe extern "C" fn(CuExternalSemaphore) -> CuResult;
#[cfg(target_os = "linux")]
type CuWaitExternalSemaphoresAsync = unsafe extern "C" fn(
    *const CuExternalSemaphore,
    *const CudaExternalSemaphoreWaitParams,
    u32,
    CuStream,
) -> CuResult;
#[cfg(target_os = "linux")]
type CuSignalExternalSemaphoresAsync = unsafe extern "C" fn(
    *const CuExternalSemaphore,
    *const CudaExternalSemaphoreSignalParams,
    u32,
    CuStream,
) -> CuResult;
#[cfg(target_os = "linux")]
type CuStreamSynchronize = unsafe extern "C" fn(CuStream) -> CuResult;

#[cfg(target_os = "linux")]
struct CudaDriver {
    handle: *mut c_void,
    init: CuInit,
    device_get: CuDeviceGet,
    device_get_uuid: CuDeviceGetUuid,
    primary_retain: CuDevicePrimaryCtxRetain,
    primary_release: CuDevicePrimaryCtxRelease,
    set_current: CuCtxSetCurrent,
    error_name: CuGetErrorName,
    import_memory: CuImportExternalMemory,
    map_buffer: CuMapExternalBuffer,
    destroy_memory: CuDestroyExternalMemory,
    import_semaphore: CuImportExternalSemaphore,
    destroy_semaphore: CuDestroyExternalSemaphore,
    wait_semaphore: CuWaitExternalSemaphoresAsync,
    signal_semaphore: CuSignalExternalSemaphoresAsync,
    stream_synchronize: CuStreamSynchronize,
}
#[cfg(target_os = "linux")]
unsafe impl Send for CudaDriver {}
#[cfg(target_os = "linux")]
unsafe impl Sync for CudaDriver {}
#[cfg(target_os = "linux")]
impl CudaDriver {
    fn load() -> Result<Arc<Self>> {
        let lib = CString::new("libcuda.so.1").unwrap();
        let handle = unsafe { dlopen(lib.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            bail!("CUDA driver (libcuda.so.1) unavailable");
        }
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                let name = CString::new($name).unwrap();
                let p = unsafe { dlsym(handle, name.as_ptr()) };
                if p.is_null() {
                    bail!("CUDA driver lacks required symbol {}", $name);
                }
                unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
            }};
        }
        Ok(Arc::new(Self {
            handle,
            init: symbol!("cuInit", CuInit),
            device_get: symbol!("cuDeviceGet", CuDeviceGet),
            device_get_uuid: symbol!("cuDeviceGetUuid", CuDeviceGetUuid),
            primary_retain: symbol!("cuDevicePrimaryCtxRetain", CuDevicePrimaryCtxRetain),
            primary_release: symbol!("cuDevicePrimaryCtxRelease", CuDevicePrimaryCtxRelease),
            set_current: symbol!("cuCtxSetCurrent", CuCtxSetCurrent),
            error_name: symbol!("cuGetErrorName", CuGetErrorName),
            import_memory: symbol!("cuImportExternalMemory", CuImportExternalMemory),
            map_buffer: symbol!("cuExternalMemoryGetMappedBuffer", CuMapExternalBuffer),
            destroy_memory: symbol!("cuDestroyExternalMemory", CuDestroyExternalMemory),
            import_semaphore: symbol!("cuImportExternalSemaphore", CuImportExternalSemaphore),
            destroy_semaphore: symbol!("cuDestroyExternalSemaphore", CuDestroyExternalSemaphore),
            wait_semaphore: symbol!(
                "cuWaitExternalSemaphoresAsync",
                CuWaitExternalSemaphoresAsync
            ),
            signal_semaphore: symbol!(
                "cuSignalExternalSemaphoresAsync",
                CuSignalExternalSemaphoresAsync
            ),
            stream_synchronize: symbol!("cuStreamSynchronize", CuStreamSynchronize),
        }))
    }
    fn name(&self, code: CuResult) -> String {
        let mut p = std::ptr::null();
        if unsafe { (self.error_name)(code, &mut p) } == 0 && !p.is_null() {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        } else {
            "unknown".into()
        }
    }
}
#[cfg(target_os = "linux")]
pub fn cuda_driver_available() -> bool {
    CudaDriver::load().is_ok()
}
#[cfg(target_os = "linux")]
pub fn cuda_device_uuid_matches(uuid: &[u8; 16]) -> bool {
    let Ok(d) = CudaDriver::load() else {
        return false;
    };
    if unsafe { (d.init)(0) } != 0 {
        return false;
    };
    let mut device = 0;
    if unsafe { (d.device_get)(&mut device, 0) } != 0 {
        return false;
    };
    let mut got = CudaUuid { bytes: [0; 16] };
    unsafe { (d.device_get_uuid)(&mut got, device) == 0 && got.bytes == *uuid }
}
#[cfg(target_os = "linux")]
pub fn cuda_device_matches(vendor: u32, id: u32) -> bool {
    if vendor != 0x10DE {
        return false;
    }
    let Ok(d) = CudaDriver::load() else {
        return false;
    };
    if unsafe { (d.init)(0) } != 0 {
        return false;
    };
    let mut device = 0;
    if unsafe { (d.device_get)(&mut device, 0) } != 0 {
        return false;
    };
    type Get = unsafe extern "C" fn(*mut i32, i32, CuDevice) -> CuResult;
    let n = CString::new("cuDeviceGetAttribute").unwrap();
    let p = unsafe { dlsym(d.handle, n.as_ptr()) };
    if p.is_null() {
        return false;
    }
    let get: Get = unsafe { std::mem::transmute(p) };
    let mut got = 0;
    unsafe { get(&mut got, 34, device) == 0 && got as u32 == id }
}
#[cfg(target_os = "linux")]
pub fn nvenc_available() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout).lines().any(|l| {
                let mut f = l.split_whitespace();
                f.next().is_some() && f.next() == Some("h264_nvenc")
            })
        })
}

#[cfg(target_os = "linux")]
const _: () = {
    assert!(std::mem::size_of::<CudaExternalMemoryHandleDesc>() == 104);
    assert!(std::mem::align_of::<CudaExternalMemoryHandleDesc>() == 8);
    assert!(std::mem::size_of::<CudaExternalMemoryBufferDesc>() == 88);
    assert!(std::mem::size_of::<CudaExternalSemaphoreHandleDesc>() == 96);
    assert!(std::mem::size_of::<CudaExternalSemaphoreWaitParams>() == 144);
    assert!(std::mem::size_of::<CudaExternalSemaphoreSignalParams>() == 144);
};

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Nv12Layout {
    pub width: u32,
    pub height: u32,
    pub y_pitch: usize,
    pub uv_pitch: usize,
    pub y_offset: usize,
    pub uv_offset: usize,
    pub size: usize,
}
#[cfg(target_os = "linux")]
impl Nv12Layout {
    pub fn new(width: u32, height: u32, y_pitch: usize, uv_pitch: usize) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("NV12 dimensions must be positive")
        }
        if width % 2 != 0 || height % 2 != 0 {
            bail!("zero-copy NV12 requires even dimensions, got {width}x{height}")
        }
        if y_pitch < width as usize || uv_pitch < width as usize {
            bail!("NV12 pitches are smaller than the image width")
        }
        let y = y_pitch
            .checked_mul(height as usize)
            .context("NV12 Y plane size overflow")?;
        let uv = uv_pitch
            .checked_mul((height / 2) as usize)
            .context("NV12 UV plane size overflow")?;
        Ok(Self {
            width,
            height,
            y_pitch,
            uv_pitch,
            y_offset: 0,
            uv_offset: y,
            size: y.checked_add(uv).context("NV12 buffer size overflow")?,
        })
    }
    pub fn tight(w: u32, h: u32) -> Result<Self> {
        Self::new(w, h, w as usize, w as usize)
    }
    pub fn nvenc(w: u32, h: u32) -> Result<Self> {
        let p = (w as usize).next_multiple_of(512);
        Self::new(w, h, p, p)
    }
}
#[cfg(target_os = "linux")]
pub struct CudaNv12Mapping {
    external: CuExternalMemory,
    pointer: u64,
    size: u64,
    context: CuContext,
    device: CuDevice,
    driver: Arc<CudaDriver>,
}
#[cfg(target_os = "linux")]
unsafe impl Send for CudaNv12Mapping {}
#[cfg(target_os = "linux")]
unsafe impl Sync for CudaNv12Mapping {}
#[cfg(target_os = "linux")]
impl CudaNv12Mapping {
    pub fn pointer(&self) -> u64 {
        self.pointer
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn context(&self) -> *mut c_void {
        self.context
    }
    fn validate(&self, l: Nv12Layout) -> Result<()> {
        if self.pointer == 0 || self.size < l.size as u64 {
            bail!("CUDA NV12 mapping does not contain the complete allocation")
        }
        let y = l
            .y_offset
            .checked_add(
                l.y_pitch
                    .checked_mul(l.height as usize)
                    .context("NV12 Y bounds overflow")?,
            )
            .context("NV12 Y bounds overflow")?;
        let uv = l
            .uv_offset
            .checked_add(
                l.uv_pitch
                    .checked_mul((l.height / 2) as usize)
                    .context("NV12 UV bounds overflow")?,
            )
            .context("NV12 UV bounds overflow")?;
        if l.y_offset > l.uv_offset || y > l.uv_offset || uv > self.size as usize {
            bail!("NV12 plane bounds exceed the CUDA mapping")
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
impl Drop for CudaNv12Mapping {
    fn drop(&mut self) {
        unsafe {
            (self.driver.destroy_memory)(self.external);
            (self.driver.primary_release)(self.device);
        }
    }
}
#[cfg(target_os = "linux")]
pub fn import_cuda_nv12_memory(fd: OwnedFd, size: usize) -> Result<CudaNv12Mapping> {
    if size == 0 {
        bail!("cannot import zero-sized CUDA NV12 memory")
    }
    let d = CudaDriver::load()?;
    let r = unsafe { (d.init)(0) };
    if r != 0 {
        bail!("CUDA cuInit failed ({r}, {})", d.name(r))
    }
    let mut dev = 0;
    let r = unsafe { (d.device_get)(&mut dev, 0) };
    if r != 0 {
        bail!("CUDA cuDeviceGet failed ({r}, {})", d.name(r))
    }
    let mut ctx = std::ptr::null_mut();
    let r = unsafe { (d.primary_retain)(&mut ctx, dev) };
    if r != 0 || ctx.is_null() {
        bail!("CUDA primary context retain failed ({r}, {})", d.name(r))
    }
    let r = unsafe { (d.set_current)(ctx) };
    if r != 0 {
        unsafe { (d.primary_release)(dev) };
        bail!(
            "CUDA primary context activation failed ({r}, {})",
            d.name(r)
        )
    }
    let desc = CudaExternalMemoryHandleDesc {
        type_: 1,
        handle: CudaExternalMemoryHandle { fd: fd.as_raw_fd() },
        size: size as u64,
        flags: 0,
        reserved: [0; 16],
    };
    let mut ext = std::ptr::null_mut();
    let r = unsafe { (d.import_memory)(&mut ext, &desc) };
    if r != 0 || ext.is_null() {
        unsafe { (d.primary_release)(dev) };
        bail!("CUDA external-memory import failed ({r}, {})", d.name(r))
    }
    let _ = fd.into_raw_fd();
    let map = CudaExternalMemoryBufferDesc {
        offset: 0,
        size: size as u64,
        flags: 0,
        reserved: [0; 16],
    };
    let mut ptr = 0;
    let r = unsafe { (d.map_buffer)(&mut ptr, ext, &map) };
    if r != 0 || ptr == 0 {
        unsafe {
            (d.destroy_memory)(ext);
            (d.primary_release)(dev)
        };
        bail!("CUDA external-memory mapping failed ({r}, {})", d.name(r))
    }
    Ok(CudaNv12Mapping {
        external: ext,
        pointer: ptr,
        size: size as u64,
        context: ctx,
        device: dev,
        driver: d,
    })
}

#[cfg(target_os = "linux")]
pub struct CudaExternalSemaphore {
    semaphore: CuExternalSemaphore,
    context: CuContext,
    driver: Arc<CudaDriver>,
}
#[cfg(target_os = "linux")]
unsafe impl Send for CudaExternalSemaphore {}
#[cfg(target_os = "linux")]
unsafe impl Sync for CudaExternalSemaphore {}
impl CudaExternalSemaphore {
    pub fn wait(&self, stream: CuStream, frame: u64, slot: usize) -> Result<()> {
        let p = CudaExternalSemaphoreWaitParams {
            params: [0; 9],
            flags: 0,
            reserved: [0; 16],
        };
        let r = unsafe { (self.driver.set_current)(self.context) };
        if r != 0 {
            bail!(
                "CUDA semaphore wait context activation failed for frame {frame}, slot {slot} ({r})"
            )
        }
        let r = unsafe { (self.driver.wait_semaphore)(&self.semaphore, &p, 1, stream) };
        if r != 0 {
            bail!(
                "CUDA semaphore wait failed for frame {frame}, slot {slot} ({r}, {})",
                self.driver.name(r)
            )
        }
        let r = unsafe { (self.driver.stream_synchronize)(stream) };
        if r != 0 {
            bail!(
                "CUDA semaphore wait synchronization failed for frame {frame}, slot {slot} ({r}, {})",
                self.driver.name(r)
            )
        }
        eprintln!("zero-copy CUDA external semaphore wait complete: frame={frame} slot={slot}");
        Ok(())
    }
}
#[cfg(target_os = "linux")]
impl Drop for CudaExternalSemaphore {
    fn drop(&mut self) {
        unsafe {
            (self.driver.destroy_semaphore)(self.semaphore);
        }
    }
}
#[cfg(target_os = "linux")]
pub fn import_cuda_external_semaphore(
    fd: OwnedFd,
    ctx: CuContext,
) -> Result<CudaExternalSemaphore> {
    if ctx.is_null() {
        bail!("cannot import CUDA semaphore without a context")
    }
    let d = CudaDriver::load()?;
    let desc = CudaExternalSemaphoreHandleDesc {
        type_: 1,
        handle: CudaExternalSemaphoreHandle { fd: fd.as_raw_fd() },
        flags: 0,
        reserved: [0; 16],
    };
    let mut sem = std::ptr::null_mut();
    let r = unsafe { (d.import_semaphore)(&mut sem, &desc) };
    if r != 0 || sem.is_null() {
        bail!("CUDA external-semaphore import failed ({r}, {})", d.name(r))
    }
    let _ = fd.into_raw_fd();
    Ok(CudaExternalSemaphore {
        semaphore: sem,
        context: ctx,
        driver: d,
    })
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct GpuFramePayload {
    mapping: Arc<CudaNv12Mapping>,
    semaphore: Arc<CudaExternalSemaphore>,
    token: Arc<SlotToken>,
    layout: Nv12Layout,
    slot_index: usize,
}
#[cfg(target_os = "linux")]
impl GpuFramePayload {
    pub fn new(
        mapping: Arc<CudaNv12Mapping>,
        semaphore: Arc<CudaExternalSemaphore>,
        token: Arc<SlotToken>,
        layout: Nv12Layout,
        slot_index: usize,
    ) -> Result<Self> {
        mapping.validate(layout)?;
        Ok(Self {
            mapping,
            semaphore,
            token,
            layout,
            slot_index,
        })
    }
    pub fn layout(&self) -> Nv12Layout {
        self.layout
    }
    pub fn pointer(&self) -> u64 {
        self.mapping.pointer()
    }
    pub fn mapping_size(&self) -> u64 {
        self.mapping.size()
    }
    pub fn context(&self) -> CuContext {
        self.mapping.context()
    }
    pub fn wait_for_cuda(&self, frame: u64) -> Result<()> {
        self.semaphore
            .wait(std::ptr::null_mut(), frame, self.slot_index)?;
        self.token
            .transition(SlotState::VulkanWriting, SlotState::CudaReady)
    }
    pub fn claim_encoder(&self) -> Result<()> {
        self.token
            .transition(SlotState::CudaReady, SlotState::EncoderOwned)
    }
}
#[cfg(target_os = "linux")]
impl Drop for GpuFramePayload {
    fn drop(&mut self) {
        self.token.release_if_owned();
    }
}
#[cfg(target_os = "linux")]
pub struct SlotToken {
    state: std::sync::atomic::AtomicU8,
}
#[cfg(target_os = "linux")]
impl SlotToken {
    pub fn new() -> Self {
        Self {
            state: std::sync::atomic::AtomicU8::new(0),
        }
    }
    pub fn state(&self) -> SlotState {
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            1 => SlotState::VulkanWriting,
            2 => SlotState::CudaReady,
            3 => SlotState::EncoderOwned,
            _ => SlotState::Free,
        }
    }
    pub fn transition(&self, expected: SlotState, next: SlotState) -> Result<()> {
        self.state
            .compare_exchange(
                expected as u8,
                next as u8,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| {
                anyhow::anyhow!(
                    "invalid zero-copy slot transition: {:?} -> {next:?}",
                    self.state()
                )
            })
    }
    fn release_if_owned(&self) {
        let _ = self.state.compare_exchange(
            3,
            0,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }
}
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotState {
    Free = 0,
    VulkanWriting = 1,
    CudaReady = 2,
    EncoderOwned = 3,
}
#[cfg(target_os = "linux")]
impl SlotState {
    pub fn transition(self, next: Self) -> Result<Self> {
        if matches!(
            (self, next),
            (Self::Free, Self::VulkanWriting)
                | (Self::VulkanWriting, Self::CudaReady)
                | (Self::CudaReady, Self::EncoderOwned)
                | (Self::EncoderOwned, Self::Free)
        ) {
            Ok(next)
        } else {
            bail!("invalid zero-copy slot transition: {self:?} -> {next:?}")
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityInputs {
    pub renderer_is_vulkan: bool,
    pub external_memory_fd: bool,
    pub external_semaphore_fd: bool,
    pub cuda_driver: bool,
    pub nvenc: bool,
    pub matching_device: bool,
    pub shader_spirv: bool,
}
#[cfg(target_os = "linux")]
pub fn negotiate(i: CapabilityInputs) -> Result<()> {
    let mut m = Vec::new();
    if !i.renderer_is_vulkan {
        m.push("Vulkan renderer")
    }
    if !i.external_memory_fd {
        m.push("VK_KHR_external_memory_fd")
    }
    if !i.external_semaphore_fd {
        m.push("VK_KHR_external_semaphore_fd")
    }
    if !i.cuda_driver {
        m.push("CUDA driver (libcuda.so.1)")
    }
    if !i.nvenc {
        m.push("h264_nvenc")
    }
    if !i.matching_device {
        m.push("matching Vulkan/CUDA physical-device identity")
    }
    if !i.shader_spirv {
        m.push("compiled rgba_to_nv12.comp.spv (glslc)")
    }
    if m.is_empty() {
        Ok(())
    } else {
        bail!(
            "Vulkan/CUDA/NVENC zero-copy capability unavailable: {}",
            m.join(", ")
        )
    }
}
#[cfg(target_os = "linux")]
pub struct ExternalNv12Slot {
    layout: Nv12Layout,
    memory_fd: OwnedFd,
    semaphore_fd: OwnedFd,
    state: SlotState,
}
#[cfg(target_os = "linux")]
impl ExternalNv12Slot {
    pub unsafe fn from_exported_fds(
        layout: Nv12Layout,
        memory_fd: RawFd,
        semaphore_fd: RawFd,
    ) -> Self {
        Self {
            layout,
            memory_fd: unsafe { OwnedFd::from_raw_fd(memory_fd) },
            semaphore_fd: unsafe { OwnedFd::from_raw_fd(semaphore_fd) },
            state: SlotState::Free,
        }
    }
    pub fn layout(&self) -> Nv12Layout {
        self.layout
    }
    pub fn state(&self) -> SlotState {
        self.state
    }
    pub fn memory_fd(&self) -> RawFd {
        self.memory_fd.as_raw_fd()
    }
    pub fn semaphore_fd(&self) -> RawFd {
        self.semaphore_fd.as_raw_fd()
    }
    pub fn transition(&mut self, next: SlotState) -> Result<()> {
        self.state = self.state.transition(next)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::{CapabilityInputs, Nv12Layout, SlotState, negotiate};
    #[cfg(target_os = "linux")]
    #[test]
    fn nv12_layout_has_contiguous_planes() {
        let l = Nv12Layout::tight(4, 2).unwrap();
        assert_eq!((l.y_offset, l.uv_offset, l.size), (0, 8, 12));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn nv12_rejects_odd_dimensions() {
        assert!(
            Nv12Layout::tight(3, 2)
                .unwrap_err()
                .to_string()
                .contains("even dimensions")
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn slot_state_machine_rejects_skips() {
        assert!(SlotState::Free.transition(SlotState::CudaReady).is_err());
        assert_eq!(
            SlotState::Free
                .transition(SlotState::VulkanWriting)
                .unwrap()
                .transition(SlotState::CudaReady)
                .unwrap()
                .transition(SlotState::EncoderOwned)
                .unwrap()
                .transition(SlotState::Free)
                .unwrap(),
            SlotState::Free
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn negotiation_reports_all_missing_capabilities() {
        let e = negotiate(CapabilityInputs {
            renderer_is_vulkan: false,
            external_memory_fd: false,
            external_semaphore_fd: false,
            cuda_driver: false,
            nvenc: false,
            matching_device: false,
            shader_spirv: false,
        })
        .unwrap_err()
        .to_string();
        for s in [
            "Vulkan renderer",
            "external_memory_fd",
            "external_semaphore_fd",
            "CUDA driver",
            "h264_nvenc",
            "physical-device",
        ] {
            assert!(e.contains(s));
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn nvenc_layout_uses_aligned_contiguous_planes() {
        let layout = Nv12Layout::nvenc(1920, 1080).unwrap();
        assert_eq!(layout.y_pitch, 2048);
        assert_eq!(layout.uv_offset, 2048 * 1080);
        assert_eq!(layout.size, 2048 * 1080 + 2048 * 540);
    }
}
