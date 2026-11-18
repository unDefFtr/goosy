#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {}

use anyhow::{Context, Result, bail};
use skia_safe::{Canvas, IPoint, ImageInfo, Surface, surfaces};

pub struct SurfaceRenderer {
    backend: Backend,
}

enum Backend {
    Raster,
    #[cfg(target_os = "linux")]
    Vulkan(VulkanBackend),
    #[cfg(target_os = "windows")]
    Direct3D(Direct3DBackend),
    #[cfg(target_os = "windows")]
    Angle(AngleBackend),
    #[cfg(target_os = "macos")]
    Metal(MetalBackend),
}
const GPU_RESOURCE_CACHE_BYTES: usize = 256 * 1024 * 1024;

impl SurfaceRenderer {
    pub fn new() -> Self {
        Self::try_new().unwrap_or_else(|error| {
            eprintln!("goosy: GPU backend unavailable; using raster: {error:#}");
            Self {
                backend: Backend::Raster,
            }
        })
    }

    pub fn try_new() -> Result<Self> {
        let requested = requested_backend()?;
        if requested == BackendRequest::Raster {
            return Ok(Self {
                backend: Backend::Raster,
            });
        }
        #[cfg(target_os = "linux")]
        if requested.allows_vulkan() {
            match VulkanBackend::new() {
                Ok(vulkan) => {
                    return Ok(Self {
                        backend: Backend::Vulkan(vulkan),
                    });
                }
                Err(error) if requested == BackendRequest::Vulkan => {
                    return Err(error.context("initialize Vulkan backend"));
                }
                Err(error) => {
                    eprintln!("goosy: Vulkan unavailable; using raster: {error:#}");
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            if requested.allows_d3d12() {
                match Direct3DBackend::new() {
                    Ok(direct3d) => {
                        return Ok(Self {
                            backend: Backend::Direct3D(direct3d),
                        });
                    }
                    Err(error) if requested == BackendRequest::D3d12 => {
                        return Err(error.context("initialize D3D12 backend"));
                    }
                    Err(error) => eprintln!("goosy: D3D12 unavailable; trying ANGLE: {error:#}"),
                }
            }
            if requested.allows_angle() {
                match AngleBackend::new() {
                    Some(angle) => {
                        return Ok(Self {
                            backend: Backend::Angle(angle),
                        });
                    }
                    None if requested == BackendRequest::Angle => {
                        bail!("initialize ANGLE backend (libEGL.dll not available or unsupported)");
                    }
                    None => eprintln!("goosy: ANGLE unavailable; using raster"),
                }
            }
        }
        #[cfg(target_os = "macos")]
        if requested.allows_metal() {
            if let Some(metal) = MetalBackend::new() {
                return Ok(Self {
                    backend: Backend::Metal(metal),
                });
            }
            if requested == BackendRequest::Metal {
                bail!("initialize Metal backend");
            }
            eprintln!("goosy: Metal unavailable; using raster");
        }
        if requested != BackendRequest::Auto {
            bail!("requested GPU backend is unavailable: {requested:?}");
        }
        Ok(Self {
            backend: Backend::Raster,
        })
    }

    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            Backend::Raster => "raster",
            #[cfg(target_os = "linux")]
            Backend::Vulkan(_) => "vulkan",
            #[cfg(target_os = "windows")]
            Backend::Direct3D(_) => "d3d12",
            #[cfg(target_os = "windows")]
            Backend::Angle(_) => "angle",
            #[cfg(target_os = "macos")]
            Backend::Metal(_) => "metal",
        }
    }

    pub fn backend_details(&self) -> String {
        match &self.backend {
            Backend::Raster => "CPU raster (RGBA_8888 premultiplied sRGB)".to_owned(),
            #[cfg(target_os = "linux")]
            Backend::Vulkan(vulkan) => vulkan.details(),
            #[cfg(target_os = "windows")]
            Backend::Direct3D(direct3d) => direct3d.details(),
            #[cfg(target_os = "windows")]
            Backend::Angle(_) => "ANGLE OpenGL ES (libEGL.dll)".to_owned(),
            #[cfg(target_os = "macos")]
            Backend::Metal(_) => "Skia Ganesh Metal".to_owned(),
        }
    }

    pub fn render_into<F>(
        &mut self,
        info: &ImageInfo,
        row_bytes: usize,
        pixels: &mut Vec<u8>,
        draw: F,
    ) -> Result<()>
    where
        F: FnOnce(&Canvas) -> Result<()>,
    {
        match &mut self.backend {
            Backend::Raster => render_raster_into(info, row_bytes, pixels, draw),
            #[cfg(target_os = "linux")]
            Backend::Vulkan(vulkan) => vulkan.render_into(info, row_bytes, pixels, draw),
            #[cfg(target_os = "windows")]
            Backend::Direct3D(direct3d) => direct3d.render_into(info, row_bytes, pixels, draw),
            #[cfg(target_os = "windows")]
            Backend::Angle(angle) => angle.render_into(info, row_bytes, pixels, draw),
            #[cfg(target_os = "macos")]
            Backend::Metal(metal) => metal.render_into(info, row_bytes, pixels, draw),
        }
    }

    pub fn render_into_frame<F>(
        &mut self,
        info: &ImageInfo,
        row_bytes: usize,
        frame: &mut crate::frame::RenderedFrame,
        read_cpu_pixels: bool,
        draw: F,
    ) -> Result<()>
    where
        F: FnOnce(&Canvas) -> Result<()>,
    {
        #[cfg(target_os = "macos")]
        if let Backend::Metal(metal) = &mut self.backend {
            let gpu_frame = metal.render_gpu_frame(
                info,
                row_bytes,
                read_cpu_pixels.then(|| frame.pixels_mut()),
                draw,
            )?;
            frame.set_gpu_frame(gpu_frame);
            return Ok(());
        }
        let _ = read_cpu_pixels;
        self.render_into(info, row_bytes, frame.pixels_mut(), draw)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackendRequest {
    Auto,
    Raster,
    #[cfg(target_os = "linux")]
    Vulkan,
    #[cfg(target_os = "windows")]
    D3d12,
    #[cfg(target_os = "windows")]
    Angle,
    #[cfg(target_os = "macos")]
    Metal,
}

fn requested_backend() -> Result<BackendRequest> {
    let value = std::env::var_os("GOOSY_RENDER_BACKEND");
    parse_backend_request(value.as_deref())
}

fn parse_backend_request(value: Option<&std::ffi::OsStr>) -> Result<BackendRequest> {
    let value = value
        .unwrap_or_default()
        .to_string_lossy()
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "" | "auto" => Ok(BackendRequest::Auto),
        "raster" | "cpu" => Ok(BackendRequest::Raster),
        #[cfg(target_os = "linux")]
        "vulkan" | "vk" => Ok(BackendRequest::Vulkan),
        #[cfg(target_os = "windows")]
        "d3d12" | "direct3d" | "direct3d12" => Ok(BackendRequest::D3d12),
        #[cfg(target_os = "windows")]
        "angle" | "gles" | "opengles" => Ok(BackendRequest::Angle),
        #[cfg(target_os = "macos")]
        "metal" => Ok(BackendRequest::Metal),
        _ => bail!(
            "invalid GOOSY_RENDER_BACKEND={value:?}; expected auto, raster, or a platform backend"
        ),
    }
}

impl BackendRequest {
    #[cfg(target_os = "linux")]
    fn allows_vulkan(self) -> bool {
        matches!(self, Self::Auto | Self::Vulkan)
    }

    #[cfg(target_os = "windows")]
    fn allows_d3d12(self) -> bool {
        matches!(self, Self::Auto | Self::D3d12)
    }

    #[cfg(target_os = "windows")]
    fn allows_angle(self) -> bool {
        matches!(self, Self::Auto | Self::Angle)
    }

    #[cfg(target_os = "macos")]
    fn allows_metal(self) -> bool {
        matches!(self, Self::Auto | Self::Metal)
    }
}
#[cfg(target_os = "windows")]
pub fn probe_selected_backend(width: u32, height: u32) -> Result<String> {
    use skia_safe::{AlphaType, Color, ColorSpace, ColorType, ISize};

    let width = width.clamp(1, 64) as i32;
    let height = height.clamp(1, 64) as i32;
    let info = ImageInfo::new(
        ISize::new(width, height),
        ColorType::RGBA8888,
        AlphaType::Premul,
        ColorSpace::new_srgb(),
    );
    let mut renderer = SurfaceRenderer::try_new()?;
    let details = renderer.backend_details();
    let mut pixels = Vec::new();
    renderer.render_into(&info, width as usize * 4, &mut pixels, |canvas| {
        canvas.clear(Color::from_argb(255, 12, 34, 56));
        Ok(())
    })?;
    if pixels.len() != width as usize * height as usize * 4 {
        bail!("backend probe returned an invalid pixel buffer");
    }
    Ok(details)
}

fn render_raster_into<F>(
    info: &ImageInfo,
    row_bytes: usize,
    pixels: &mut Vec<u8>,
    draw: F,
) -> Result<()>
where
    F: FnOnce(&Canvas) -> Result<()>,
{
    pixels.resize(info.compute_byte_size(row_bytes), 0);
    let mut surface = surfaces::wrap_pixels(info, pixels.as_mut_slice(), row_bytes, None)
        .context("create direct raster surface")?;
    draw(surface.canvas())?;
    Ok(())
}

fn read_pixels_into(
    surface: &mut Surface,
    info: &ImageInfo,
    row_bytes: usize,
    pixels: &mut Vec<u8>,
) -> Result<()> {
    pixels.resize(info.compute_byte_size(row_bytes), 0);
    if !surface.read_pixels(info, pixels, row_bytes, IPoint::new(0, 0)) {
        bail!("read RGBA pixels from surface");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct VulkanNative {
    device: Option<ash::Device>,
    instance: Option<ash::Instance>,
    entry: ash::Entry,
}

#[cfg(target_os = "linux")]
impl Drop for VulkanNative {
    fn drop(&mut self) {
        unsafe {
            if let Some(device) = self.device.take() {
                let _ = device.device_wait_idle();
                device.destroy_device(None);
            }
            if let Some(instance) = self.instance.take() {
                instance.destroy_instance(None);
            }
        }
    }
}

#[cfg(target_os = "linux")]
struct VulkanBackend {
    surface: Option<Surface>,
    context: Option<skia_safe::gpu::DirectContext>,
    #[allow(dead_code)]
    native: VulkanNative,
    device_name: String,
    vendor_id: u32,
    device_type: ash::vk::PhysicalDeviceType,
}

#[cfg(target_os = "linux")]
impl VulkanBackend {
    fn new() -> Result<Self> {
        use ash::vk::{self, Handle};
        use skia_safe::gpu::ganesh::ResourceCacheLimits;
        use skia_safe::gpu::{self, vk as skia_vk};
        use std::ffi::{CStr, CString};

        let entry = unsafe { ash::Entry::load() }.context("load Vulkan loader")?;
        let loader_version = unsafe { entry.try_enumerate_instance_version() }
            .context("query Vulkan loader API version")?
            .unwrap_or(vk::API_VERSION_1_0);
        if loader_version < vk::API_VERSION_1_1 {
            bail!(
                "Vulkan loader API {}.{} is below required 1.1",
                vk::api_version_major(loader_version),
                vk::api_version_minor(loader_version)
            );
        }
        let api_version = loader_version.min(vk::API_VERSION_1_3);
        let application_name = CString::new("Goosy").expect("static application name");
        let application_info = vk::ApplicationInfo::default()
            .application_name(&application_name)
            .application_version(0)
            .engine_name(&application_name)
            .engine_version(0)
            .api_version(api_version);
        let instance_info = vk::InstanceCreateInfo::default().application_info(&application_info);
        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .context("create Vulkan instance")?;
        let mut native = VulkanNative {
            device: None,
            instance: Some(instance),
            entry,
        };

        let instance = native
            .instance
            .as_ref()
            .expect("Vulkan instance initialized");
        let physical_devices = unsafe { instance.enumerate_physical_devices() }
            .context("enumerate Vulkan physical devices")?;
        let mut candidates = Vec::new();
        for physical_device in physical_devices {
            let properties = unsafe { instance.get_physical_device_properties(physical_device) };
            if properties.api_version < vk::API_VERSION_1_1 {
                continue;
            }
            let queue_family_index =
                unsafe { instance.get_physical_device_queue_family_properties(physical_device) }
                    .iter()
                    .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS));
            let Some(queue_family_index) = queue_family_index else {
                continue;
            };
            let preference = match properties.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => 0,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
                vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
                vk::PhysicalDeviceType::CPU => 3,
                _ => 4,
            };
            candidates.push((
                preference,
                physical_device,
                queue_family_index as u32,
                properties,
            ));
        }
        candidates.sort_by_key(|candidate| candidate.0);
        let Some((_, physical_device, queue_family_index, properties)) =
            candidates.into_iter().next()
        else {
            bail!("no graphics-capable Vulkan 1.1 device");
        };

        let priorities = [1.0_f32];
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&priorities);
        let queue_infos = [queue_info];
        let device_info = vk::DeviceCreateInfo::default().queue_create_infos(&queue_infos);
        let device = unsafe { instance.create_device(physical_device, &device_info, None) }
            .context("create Vulkan graphics device")?;
        native.device = Some(device);
        let device = native.device.as_ref().expect("Vulkan device initialized");
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        let mut context = {
            let get_proc = |request: skia_vk::GetProcOf| unsafe {
                match request {
                    skia_vk::GetProcOf::Instance(raw_instance, name) => native
                        .entry
                        .get_instance_proc_addr(vk::Instance::from_raw(raw_instance as _), name),
                    skia_vk::GetProcOf::Device(raw_device, name) => {
                        instance.get_device_proc_addr(vk::Device::from_raw(raw_device as _), name)
                    }
                }
                .map(|function| function as *const () as *const std::ffi::c_void)
                .unwrap_or(std::ptr::null())
            };
            let backend = unsafe {
                skia_vk::BackendContext::new_builder(
                    instance.handle().as_raw() as _,
                    physical_device.as_raw() as _,
                    device.handle().as_raw() as _,
                    (queue.as_raw() as _, queue_family_index as usize),
                    &get_proc,
                    Some(api_version.into()),
                )
                .build()
            };
            gpu::direct_contexts::make_vulkan(&backend, None)
                .context("create Skia Ganesh Vulkan context")?
        };
        context.set_resource_cache_limits(ResourceCacheLimits {
            max_resources: 4_096,
            max_resource_bytes: GPU_RESOURCE_CACHE_BYTES,
        });
        let device_name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();

        Ok(Self {
            surface: None,
            context: Some(context),
            native,
            device_name,
            vendor_id: properties.vendor_id,
            device_type: properties.device_type,
        })
    }

    fn details(&self) -> String {
        format!(
            "Skia Ganesh Vulkan / {} (vendor 0x{:04X}, {:?})",
            self.device_name, self.vendor_id, self.device_type
        )
    }

    fn render_into<F>(
        &mut self,
        info: &ImageInfo,
        row_bytes: usize,
        pixels: &mut Vec<u8>,
        draw: F,
    ) -> Result<()>
    where
        F: FnOnce(&Canvas) -> Result<()>,
    {
        use skia_safe::gpu::{self, Budgeted, SurfaceOrigin};

        let context = self.context.as_mut().expect("Vulkan context initialized");
        if self.surface.is_none() {
            self.surface = Some(
                gpu::surfaces::render_target(
                    context,
                    Budgeted::Yes,
                    info,
                    0,
                    SurfaceOrigin::TopLeft,
                    None,
                    false,
                    false,
                )
                .context("create Vulkan render target")?,
            );
        }
        let surface = self.surface.as_mut().expect("Vulkan surface initialized");
        draw(surface.canvas())?;
        context.flush_submit_and_sync_cpu();
        if context.is_device_lost() {
            bail!("Vulkan GPU submission failed: device lost");
        }
        if context.oomed() {
            bail!("Vulkan GPU submission failed: out of memory");
        }
        read_pixels_into(surface, info, row_bytes, pixels)
            .context("read RGBA pixels from Vulkan surface")
    }
}

#[cfg(target_os = "linux")]
impl Drop for VulkanBackend {
    fn drop(&mut self) {
        self.surface.take();
        self.context.take();
    }
}

#[cfg(target_os = "windows")]
struct Direct3DBackend {
    surface: Option<Surface>,
    // Rust drops fields in declaration order. Skia must release all D3D
    // references before the client-owned backend context and device.
    context: skia_safe::gpu::DirectContext,
    _backend: skia_safe::gpu::d3d::BackendContext,
    device: windows::Win32::Graphics::Direct3D12::ID3D12Device,
    adapter_name: String,
    vendor_id: u32,
    dedicated_video_memory: usize,
}

#[cfg(target_os = "windows")]
impl Direct3DBackend {
    fn new() -> Result<Self> {
        use skia_safe::gpu::ganesh::ResourceCacheLimits;
        use skia_safe::gpu::{self, Protected};

        let selection = select_d3d_adapter().context("find a D3D12-capable hardware adapter")?;
        let queue_desc = windows::Win32::Graphics::Direct3D12::D3D12_COMMAND_QUEUE_DESC {
            Type: windows::Win32::Graphics::Direct3D12::D3D12_COMMAND_LIST_TYPE_DIRECT,
            Priority: windows::Win32::Graphics::Direct3D12::D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
            Flags: windows::Win32::Graphics::Direct3D12::D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        };
        let queue = unsafe { selection.device.CreateCommandQueue(&queue_desc) }
            .context("create D3D12 direct command queue")?;
        let backend = skia_safe::gpu::d3d::BackendContext {
            adapter: selection.adapter,
            device: selection.device.clone(),
            queue,
            memory_allocator: None,
            protected_context: Protected::No,
        };
        let mut context = unsafe { gpu::direct_contexts::make_d3d(&backend, None) }
            .context("create Skia Ganesh D3D12 context")?;
        // Bound transient GPU resources during long renders.
        context.set_resource_cache_limits(ResourceCacheLimits {
            max_resources: 4_096,
            max_resource_bytes: GPU_RESOURCE_CACHE_BYTES,
        });
        Ok(Self {
            surface: None,
            context,
            _backend: backend,
            device: selection.device,
            adapter_name: selection.name,
            vendor_id: selection.vendor_id,
            dedicated_video_memory: selection.dedicated_video_memory,
        })
    }

    fn details(&self) -> String {
        format!(
            "D3D12 / {} (vendor 0x{:04X}, {} MiB VRAM)",
            self.adapter_name,
            self.vendor_id,
            self.dedicated_video_memory / (1024 * 1024)
        )
    }

    fn render_into<F>(
        &mut self,
        info: &ImageInfo,
        row_bytes: usize,
        pixels: &mut Vec<u8>,
        draw: F,
    ) -> Result<()>
    where
        F: FnOnce(&Canvas) -> Result<()>,
    {
        use skia_safe::gpu::{self, Budgeted, SurfaceOrigin};

        if self.surface.is_none() {
            self.surface = Some(
                gpu::surfaces::render_target(
                    &mut self.context,
                    Budgeted::Yes,
                    info,
                    0,
                    SurfaceOrigin::TopLeft,
                    None,
                    false,
                    false,
                )
                .context("create native D3D12 render target")?,
            );
        }
        let surface = self.surface.as_mut().expect("D3D12 surface initialized");
        draw(surface.canvas())?;
        // Readback must not race an asynchronous D3D12 queue submission. On
        // Windows this synchronization is also the point where device removal
        // becomes observable instead of leaving the GUI at 0% indefinitely.
        self.context.flush_submit_and_sync_cpu();
        if self.context.is_device_lost() || self.context.oomed() {
            let reason = unsafe { self.device.GetDeviceRemovedReason() }
                .map(|_| "unknown device state".to_owned())
                .unwrap_or_else(|error| format!("device removed ({error})"));
            bail!("D3D12 GPU submission failed: {reason}");
        }
        read_pixels_into(surface, info, row_bytes, pixels)
            .context("read RGBA pixels from D3D12 surface")
    }
}

#[cfg(target_os = "windows")]
struct D3dSelection {
    adapter: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: windows::Win32::Graphics::Direct3D12::ID3D12Device,
    name: String,
    vendor_id: u32,
    dedicated_video_memory: usize,
}

#[cfg(target_os = "windows")]
fn select_d3d_adapter() -> Option<D3dSelection> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIFactory6};

    if let Ok(factory) = unsafe { CreateDXGIFactory1::<IDXGIFactory6>() } {
        if let Some(selected) = select_d3d_adapter_by_preference(&factory) {
            return Some(selected);
        }
    }
    let factory = unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.ok()?;
    select_d3d_adapter_in_order(&factory)
}

#[cfg(target_os = "windows")]
fn select_d3d_adapter_by_preference(
    factory: &windows::Win32::Graphics::Dxgi::IDXGIFactory6,
) -> Option<D3dSelection> {
    use windows::Win32::Graphics::Dxgi::{DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, IDXGIAdapter1};

    select_d3d_adapter_candidates(|index| unsafe {
        factory
            .EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                index,
                DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
            )
            .ok()
    })
}

#[cfg(target_os = "windows")]
fn select_d3d_adapter_in_order(
    factory: &windows::Win32::Graphics::Dxgi::IDXGIFactory1,
) -> Option<D3dSelection> {
    select_d3d_adapter_candidates(|index| unsafe { factory.EnumAdapters1(index).ok() })
}

#[cfg(target_os = "windows")]
fn select_d3d_adapter_candidates<F>(mut enumerate: F) -> Option<D3dSelection>
where
    F: FnMut(u32) -> Option<windows::Win32::Graphics::Dxgi::IDXGIAdapter1>,
{
    use windows::Win32::Graphics::{
        Direct3D::D3D_FEATURE_LEVEL_11_0,
        Direct3D12::{D3D12CreateDevice, ID3D12Device},
        Dxgi::{DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_REMOTE, DXGI_ADAPTER_FLAG_SOFTWARE},
    };

    let mut best: Option<(u64, D3dSelection)> = None;
    for index in 0.. {
        let Some(adapter) = enumerate(index) else {
            break;
        };
        let Ok(description) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        let flags = DXGI_ADAPTER_FLAG(description.Flags as i32);
        if flags.contains(DXGI_ADAPTER_FLAG_SOFTWARE)
            || flags.contains(DXGI_ADAPTER_FLAG_REMOTE)
            || description.VendorId == 0x1414
        {
            continue;
        }
        let mut device: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }.is_err() {
            continue;
        }
        let Some(device) = device else { continue };
        let name = String::from_utf16_lossy(
            &description.Description[..description
                .Description
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(description.Description.len())],
        );
        // DXGI's HIGH_PERFORMANCE ordering captures the OS/user GPU preference.
        let score = (description.DedicatedVideoMemory as u64).min(1u64 << 50)
            + description.SharedSystemMemory as u64 / 16;
        let candidate = D3dSelection {
            adapter,
            device,
            name,
            vendor_id: description.VendorId,
            dedicated_video_memory: description.DedicatedVideoMemory,
        };
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, candidate));
        }
    }
    best.map(|(_, selection)| selection)
}

#[cfg(target_os = "windows")]
struct AngleBackend {
    surface: Option<Surface>,
    context: Option<skia_safe::gpu::DirectContext>,
    egl_context: Option<khronos_egl::Context>,
    egl_surface: Option<khronos_egl::Surface>,
    display: Option<khronos_egl::Display>,
    egl: khronos_egl::DynamicInstance<khronos_egl::EGL1_5>,
}

#[cfg(target_os = "windows")]
impl AngleBackend {
    fn new() -> Option<Self> {
        use khronos_egl as egl;
        use skia_safe::gpu;
        use std::ffi::c_void;

        let egl = load_angle_egl()?;
        let display = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }?;
        egl.initialize(display).ok()?;
        egl.bind_api(egl::OPENGL_ES_API).ok()?;

        let config_attributes = [
            egl::SURFACE_TYPE,
            egl::PBUFFER_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES3_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &config_attributes)
            .ok()??;
        let context_attributes = [egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE];
        let egl_context = egl
            .create_context(display, config, None, &context_attributes)
            .ok()?;
        let pbuffer_attributes = [egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE];
        let egl_surface = egl
            .create_pbuffer_surface(display, config, &pbuffer_attributes)
            .ok()?;
        egl.make_current(
            display,
            Some(egl_surface),
            Some(egl_surface),
            Some(egl_context),
        )
        .ok()?;

        let interface = skia_safe::gpu::gl::Interface::new_load_with(|name| {
            egl.get_proc_address(name)
                .map(|proc| proc as *const () as *const c_void)
                .unwrap_or(std::ptr::null())
        })?;
        let mut context = gpu::direct_contexts::make_gl(interface, None)?;
        context.set_resource_cache_limits(skia_safe::gpu::ganesh::ResourceCacheLimits {
            max_resources: 4_096,
            max_resource_bytes: GPU_RESOURCE_CACHE_BYTES,
        });

        Some(Self {
            surface: None,
            context: Some(context),
            egl_context: Some(egl_context),
            egl_surface: Some(egl_surface),
            display: Some(display),
            egl,
        })
    }

    fn render_into<F>(
        &mut self,
        info: &ImageInfo,
        row_bytes: usize,
        pixels: &mut Vec<u8>,
        draw: F,
    ) -> Result<()>
    where
        F: FnOnce(&Canvas) -> Result<()>,
    {
        use skia_safe::gpu::{self, Budgeted, SurfaceOrigin};

        if self.surface.is_none() {
            let context = self.context.as_mut().expect("ANGLE context initialized");
            self.surface = Some(
                gpu::surfaces::render_target(
                    context,
                    Budgeted::No,
                    info,
                    0,
                    SurfaceOrigin::TopLeft,
                    None,
                    false,
                    false,
                )
                .context("create ANGLE render target")?,
            );
        }
        let surface = self.surface.as_mut().expect("ANGLE surface initialized");
        draw(surface.canvas())?;
        self.context
            .as_mut()
            .expect("ANGLE context initialized")
            .flush_and_submit();
        read_pixels_into(surface, info, row_bytes, pixels)
    }
}

#[cfg(target_os = "windows")]
impl Drop for AngleBackend {
    fn drop(&mut self) {
        let _ = self
            .context
            .as_mut()
            .map(|context| context.flush_and_submit());
        self.surface.take();
        self.context.take();
        if let Some(display) = self.display {
            let _ = self.egl.make_current(display, None, None, None);
            if let Some(context) = self.egl_context.take() {
                let _ = self.egl.destroy_context(display, context);
            }
            if let Some(surface) = self.egl_surface.take() {
                let _ = self.egl.destroy_surface(display, surface);
            }
            let _ = self.egl.terminate(display);
        }
    }
}

#[cfg(target_os = "windows")]
fn load_angle_egl() -> Option<khronos_egl::DynamicInstance<khronos_egl::EGL1_5>> {
    use std::path::PathBuf;

    let mut paths = Vec::with_capacity(3);
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            paths.push(directory.join("libEGL.dll"));
        }
    }
    paths.push(PathBuf::from("libEGL.dll"));
    paths.push(PathBuf::from("angle\\libEGL.dll"));
    paths.into_iter().find_map(|path| unsafe {
        khronos_egl::DynamicInstance::<khronos_egl::EGL1_5>::load_required_from_filename(path).ok()
    })
}

#[cfg(target_os = "macos")]
mod macos_metal {
    use super::{GPU_RESOURCE_CACHE_BYTES, read_pixels_into};
    use anyhow::{Context, Result, bail};
    use objc2::{
        Encode, Encoding, msg_send,
        rc::{Retained, autoreleasepool},
        runtime::{AnyClass, AnyObject, ProtocolObject},
    };
    use objc2_metal::{
        MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLPixelFormat,
    };
    use skia_safe::{
        Canvas, ColorSpace, ColorType, ImageInfo, Surface,
        gpu::{self, Budgeted, SurfaceOrigin, mtl},
    };
    use std::ffi::{CString, c_void};
    use std::ptr::{null, null_mut};

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;

        fn CFDictionaryCreateMutable(
            allocator: *const c_void,
            capacity: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *mut c_void;
        fn CFDictionarySetValue(dict: *mut c_void, key: *const c_void, value: *const c_void);
        fn CFNumberCreate(
            allocator: *const c_void,
            the_type: isize,
            value_ptr: *const c_void,
        ) -> *mut c_void;
        pub(crate) fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: *const c_void;
        static kCVPixelBufferWidthKey: *const c_void;
        static kCVPixelBufferHeightKey: *const c_void;
        static kCVPixelBufferMetalCompatibilityKey: *const c_void;
        static kCVPixelBufferIOSurfacePropertiesKey: *const c_void;

        fn CVPixelBufferPoolCreate(
            allocator: *const c_void,
            pool_attributes: *const c_void,
            pixel_buffer_attributes: *const c_void,
            pool_out: *mut *mut c_void,
        ) -> i32;
        pub(crate) fn CVPixelBufferPoolCreatePixelBuffer(
            allocator: *const c_void,
            pool: *mut c_void,
            pixel_buffer_out: *mut *mut c_void,
        ) -> i32;
        pub(crate) fn CVPixelBufferRelease(buffer: *mut c_void);
        fn CVPixelBufferGetIOSurface(buffer: *mut c_void) -> *mut c_void;

        fn CVMetalTextureCacheCreate(
            allocator: *const c_void,
            cache_attributes: *const c_void,
            metal_device: *mut c_void,
            texture_attributes: *const c_void,
            cache_out: *mut *mut c_void,
        ) -> i32;
        fn CVMetalTextureCacheCreateTextureFromImage(
            allocator: *const c_void,
            texture_cache: *mut c_void,
            source_image: *mut c_void,
            texture_attributes: *const c_void,
            pixel_format: usize,
            width: usize,
            height: usize,
            plane_index: usize,
            texture_out: *mut *mut c_void,
        ) -> i32;
        fn CVMetalTextureGetTexture(image: *mut c_void) -> *mut c_void;
        fn CVMetalTextureCacheFlush(texture_cache: *mut c_void, options: u64);
    }

    #[link(name = "IOSurface", kind = "framework")]
    unsafe extern "C" {
        fn IOSurfaceGetID(buffer: *mut c_void) -> u32;
    }

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static POOL_ACQUIRED: AtomicU64 = AtomicU64::new(0);
    static POOL_RELEASED: AtomicU64 = AtomicU64::new(0);
    static SEEN_IOSURFACE_IDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

    #[derive(Clone, Copy, Debug, Default)]
    pub struct MacosPoolTelemetry {
        pub acquired_count: u64,
        pub released_count: u64,
        pub in_flight_count: u64,
        pub unique_iosurface_count: usize,
    }

    pub fn macos_pool_telemetry() -> MacosPoolTelemetry {
        let acquired = POOL_ACQUIRED.load(AtomicOrdering::Relaxed);
        let released = POOL_RELEASED.load(AtomicOrdering::Relaxed);
        let unique = SEEN_IOSURFACE_IDS.lock().map(|ids| ids.len()).unwrap_or(0);
        MacosPoolTelemetry {
            acquired_count: acquired,
            released_count: released,
            in_flight_count: acquired.saturating_sub(released),
            unique_iosurface_count: unique,
        }
    }

    pub fn reset_macos_pool_telemetry() {
        POOL_ACQUIRED.store(0, AtomicOrdering::Relaxed);
        POOL_RELEASED.store(0, AtomicOrdering::Relaxed);
        if let Ok(mut ids) = SEEN_IOSURFACE_IDS.lock() {
            ids.clear();
        }
    }

    pub(crate) unsafe fn record_pool_acquire(pb: *mut c_void) {
        POOL_ACQUIRED.fetch_add(1, AtomicOrdering::Relaxed);
        if !pb.is_null() {
            let ios = unsafe { CVPixelBufferGetIOSurface(pb) };
            if !ios.is_null() {
                let id = unsafe { IOSurfaceGetID(ios) };
                if let Ok(mut ids) = SEEN_IOSURFACE_IDS.lock()
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
        }
    }

    pub(crate) unsafe fn release_tracked_pixel_buffer(pb: *mut c_void) {
        if !pb.is_null() {
            POOL_RELEASED.fetch_add(1, AtomicOrdering::Relaxed);
            unsafe { CVPixelBufferRelease(pb) };
        }
    }

    const K_CF_NUMBER_SINT32_TYPE: isize = 3;
    const K_CV_PIXEL_FORMAT_420_YP_CB_CR_8_BI_PLANAR_VIDEO_RANGE: i32 = 0x34323076; // '420v'
    const RING_SIZE: usize = 3;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MTLSize {
        width: usize,
        height: usize,
        depth: usize,
    }

    unsafe impl Encode for MTLSize {
        const ENCODING: Encoding = Encoding::Struct(
            "MTLSize",
            &[usize::ENCODING, usize::ENCODING, usize::ENCODING],
        );
    }

    const METAL_SHADER_SRC: &str = r#"
#include <metal_stdlib>
using namespace metal;

kernel void rgba_to_nv12(
    texture2d<float, access::read> rgba_tex [[texture(0)]],
    texture2d<float, access::write> y_tex [[texture(1)]],
    texture2d<float, access::write> uv_tex [[texture(2)]],
    uint2 gid [[thread_position_in_grid]]
) {
    uint width = y_tex.get_width();
    uint height = y_tex.get_height();
    uint x = gid.x * 2;
    uint y = gid.y * 2;
    if (x >= width || y >= height) {
        return;
    }

    float4 p00 = rgba_tex.read(uint2(x, y));
    float4 p10 = rgba_tex.read(uint2(min(x + 1, width - 1), y));
    float4 p01 = rgba_tex.read(uint2(x, min(y + 1, height - 1)));
    float4 p11 = rgba_tex.read(uint2(min(x + 1, width - 1), min(y + 1, height - 1)));

    const float3 y_coeff = float3(0.25678824, 0.50412941, 0.09790588);
    const float y_offset = 16.0 / 255.0;

    y_tex.write(float4(dot(p00.rgb, y_coeff) + y_offset, 0.0, 0.0, 1.0), uint2(x, y));
    if (x + 1 < width) {
        y_tex.write(float4(dot(p10.rgb, y_coeff) + y_offset, 0.0, 0.0, 1.0), uint2(x + 1, y));
    }
    if (y + 1 < height) {
        y_tex.write(float4(dot(p01.rgb, y_coeff) + y_offset, 0.0, 0.0, 1.0), uint2(x, y + 1));
    }
    if (x + 1 < width && y + 1 < height) {
        y_tex.write(float4(dot(p11.rgb, y_coeff) + y_offset, 0.0, 0.0, 1.0), uint2(x + 1, y + 1));
    }

    float3 avg_rgb = 0.25 * (p00.rgb + p10.rgb + p01.rgb + p11.rgb);
    const float3 u_coeff = float3(-0.14822353, -0.29099216, 0.43921569);
    const float3 v_coeff = float3(0.43921569, -0.36778824, -0.07142745);
    float u = dot(avg_rgb, u_coeff) + (128.0 / 255.0);
    float v = dot(avg_rgb, v_coeff) + (128.0 / 255.0);

    uv_tex.write(float4(u, v, 0.0, 1.0), gid);
}
"#;

    pub struct MacosGpuFrame {
        pixel_buffer: *mut c_void,
        cmd_buf: *mut c_void,
    }

    unsafe impl Send for MacosGpuFrame {}
    unsafe impl Sync for MacosGpuFrame {}

    impl MacosGpuFrame {
        pub(crate) fn wait_and_take_pixel_buffer(&mut self) -> *mut c_void {
            if !self.cmd_buf.is_null() {
                autoreleasepool(|_| {
                    let cmd: Retained<ProtocolObject<dyn MTLCommandBuffer>> = unsafe {
                        Retained::from_raw(
                            self.cmd_buf as *mut ProtocolObject<dyn MTLCommandBuffer>,
                        )
                        .expect("non-null MTLCommandBuffer")
                    };
                    cmd.waitUntilCompleted();
                });
                self.cmd_buf = null_mut();
            }
            let pb = self.pixel_buffer;
            self.pixel_buffer = null_mut();
            pb
        }
    }

    impl Drop for MacosGpuFrame {
        fn drop(&mut self) {
            if !self.cmd_buf.is_null() {
                autoreleasepool(|_| {
                    let cmd: Retained<ProtocolObject<dyn MTLCommandBuffer>> = unsafe {
                        Retained::from_raw(
                            self.cmd_buf as *mut ProtocolObject<dyn MTLCommandBuffer>,
                        )
                        .expect("non-null MTLCommandBuffer")
                    };
                    cmd.waitUntilCompleted();
                });
                self.cmd_buf = null_mut();
            }
            if !self.pixel_buffer.is_null() {
                unsafe { release_tracked_pixel_buffer(self.pixel_buffer) };
                self.pixel_buffer = null_mut();
            }
        }
    }

    struct RingSlot {
        texture: Retained<AnyObject>,
        surface: Surface,
        last_cmd_buf: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
    }

    struct Nv12Converter {
        width: u32,
        height: u32,
        pool: *mut c_void,
        tex_cache: *mut c_void,
        pipeline: Retained<AnyObject>,
        slots: Vec<RingSlot>,
        next_slot: usize,
    }

    impl Drop for Nv12Converter {
        fn drop(&mut self) {
            for slot in &mut self.slots {
                if let Some(cmd) = slot.last_cmd_buf.take() {
                    cmd.waitUntilCompleted();
                }
            }
            self.slots.clear();
            if !self.pool.is_null() {
                unsafe { CFRelease(self.pool) };
                self.pool = null_mut();
            }
            if !self.tex_cache.is_null() {
                unsafe { CFRelease(self.tex_cache) };
                self.tex_cache = null_mut();
            }
        }
    }

    unsafe fn create_nsstring(s: &str) -> Retained<AnyObject> {
        let cstr = CString::new(s).unwrap();
        let cls = AnyClass::get(c"NSString").unwrap();
        let obj: *mut AnyObject = unsafe { msg_send![cls, stringWithUTF8String: cstr.as_ptr()] };
        unsafe { Retained::retain(obj).unwrap() }
    }

    unsafe fn compile_compute_pipeline(
        device: &ProtocolObject<dyn MTLDevice>,
    ) -> Result<Retained<AnyObject>> {
        let src = unsafe { create_nsstring(METAL_SHADER_SRC) };
        let mut err: *mut AnyObject = null_mut();
        let library: *mut AnyObject = unsafe {
            msg_send![device, newLibraryWithSource: &*src, options: null::<AnyObject>(), error: &mut err]
        };
        if library.is_null() {
            bail!("failed to compile Metal RGBA->NV12 shader library");
        }
        let library = unsafe { Retained::from_raw(library).unwrap() };
        let fn_name = unsafe { create_nsstring("rgba_to_nv12") };
        let func: *mut AnyObject = unsafe { msg_send![&*library, newFunctionWithName: &*fn_name] };
        if func.is_null() {
            bail!("failed to find rgba_to_nv12 Metal kernel");
        }
        let func = unsafe { Retained::from_raw(func).unwrap() };
        let mut err: *mut AnyObject = null_mut();
        let pipeline: *mut AnyObject = unsafe {
            msg_send![device, newComputePipelineStateWithFunction: &*func, error: &mut err]
        };
        if pipeline.is_null() {
            bail!("failed to create rgba_to_nv12 compute pipeline state");
        }
        Ok(unsafe { Retained::from_raw(pipeline).unwrap() })
    }

    unsafe fn create_render_texture(
        device: &ProtocolObject<dyn MTLDevice>,
        width: u32,
        height: u32,
    ) -> Result<Retained<AnyObject>> {
        let cls = AnyClass::get(c"MTLTextureDescriptor").unwrap();
        let desc: *mut AnyObject = unsafe {
            msg_send![
                cls,
                texture2DDescriptorWithPixelFormat: MTLPixelFormat::RGBA8Unorm,
                width: width as usize,
                height: height as usize,
                mipmapped: false
            ]
        };
        // MTLTextureUsageShaderRead (1) | MTLTextureUsageRenderTarget (4) = 5
        let _: () = unsafe { msg_send![desc, setUsage: 5usize] };
        // MTLStorageModePrivate = 2
        let _: () = unsafe { msg_send![desc, setStorageMode: 2usize] };
        let tex: *mut AnyObject = unsafe { msg_send![device, newTextureWithDescriptor: desc] };
        if tex.is_null() {
            bail!("failed to allocate Metal render texture");
        }
        Ok(unsafe { Retained::from_raw(tex).unwrap() })
    }

    pub(crate) unsafe fn create_pixel_buffer_pool(width: u32, height: u32) -> Result<*mut c_void> {
        unsafe {
            let dict = CFDictionaryCreateMutable(
                null(),
                0,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let fmt = K_CV_PIXEL_FORMAT_420_YP_CB_CR_8_BI_PLANAR_VIDEO_RANGE;
            let fmt_num = CFNumberCreate(
                null(),
                K_CF_NUMBER_SINT32_TYPE,
                &fmt as *const _ as *const c_void,
            );
            let w = width as i32;
            let w_num = CFNumberCreate(
                null(),
                K_CF_NUMBER_SINT32_TYPE,
                &w as *const _ as *const c_void,
            );
            let h = height as i32;
            let h_num = CFNumberCreate(
                null(),
                K_CF_NUMBER_SINT32_TYPE,
                &h as *const _ as *const c_void,
            );
            let io_props = CFDictionaryCreateMutable(
                null(),
                0,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );

            CFDictionarySetValue(dict, kCVPixelBufferPixelFormatTypeKey, fmt_num);
            CFDictionarySetValue(dict, kCVPixelBufferWidthKey, w_num);
            CFDictionarySetValue(dict, kCVPixelBufferHeightKey, h_num);
            CFDictionarySetValue(dict, kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue);
            CFDictionarySetValue(dict, kCVPixelBufferIOSurfacePropertiesKey, io_props);

            let mut pool: *mut c_void = null_mut();
            let status = CVPixelBufferPoolCreate(null(), null(), dict, &mut pool);

            CFRelease(fmt_num);
            CFRelease(w_num);
            CFRelease(h_num);
            CFRelease(io_props);
            CFRelease(dict);

            if status != 0 || pool.is_null() {
                bail!("CVPixelBufferPoolCreate failed with status {status}");
            }
            Ok(pool)
        }
    }

    pub(super) struct MetalBackend {
        nv12: Option<Nv12Converter>,
        surface: Option<Surface>,
        context: skia_safe::gpu::DirectContext,
        _backend: skia_safe::gpu::mtl::BackendContext,
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    }

    impl Drop for MetalBackend {
        fn drop(&mut self) {
            autoreleasepool(|_| {
                self.nv12.take();
                self.surface.take();
                self.context.flush_submit_and_sync_cpu();
                self.context.free_gpu_resources();
            });
        }
    }

    impl MetalBackend {
        pub(super) fn new() -> Option<Self> {
            let device = MTLCreateSystemDefaultDevice()?;
            let queue = device.newCommandQueue()?;
            let backend = unsafe {
                mtl::BackendContext::new(
                    Retained::as_ptr(&device) as mtl::Handle,
                    Retained::as_ptr(&queue) as mtl::Handle,
                )
            };
            let mut context = gpu::direct_contexts::make_metal(&backend, None)?;
            context.set_resource_cache_limits(skia_safe::gpu::ganesh::ResourceCacheLimits {
                max_resources: 4_096,
                max_resource_bytes: GPU_RESOURCE_CACHE_BYTES,
            });
            Some(Self {
                context,
                _backend: backend,
                device,
                queue,
                surface: None,
                nv12: None,
            })
        }

        pub(super) fn render_into<F>(
            &mut self,
            info: &ImageInfo,
            row_bytes: usize,
            pixels: &mut Vec<u8>,
            draw: F,
        ) -> Result<()>
        where
            F: FnOnce(&Canvas) -> Result<()>,
        {
            autoreleasepool(|_| {
                if self
                    .surface
                    .as_ref()
                    .is_none_or(|s| s.image_info().dimensions() != info.dimensions())
                {
                    self.surface = Some(
                        gpu::surfaces::render_target(
                            &mut self.context,
                            Budgeted::No,
                            info,
                            0,
                            SurfaceOrigin::TopLeft,
                            None,
                            false,
                            false,
                        )
                        .context("create Metal render target")?,
                    );
                }
                let surface = self.surface.as_mut().expect("Metal surface initialized");
                draw(surface.canvas())?;
                read_pixels_into(surface, info, row_bytes, pixels)
            })
        }

        fn ensure_nv12_converter(&mut self, width: u32, height: u32) -> Result<()> {
            if self
                .nv12
                .as_ref()
                .is_some_and(|c| c.width == width && c.height == height)
            {
                return Ok(());
            }
            self.nv12 = None;
            let pool = unsafe { create_pixel_buffer_pool(width, height)? };
            let mut tex_cache: *mut c_void = null_mut();
            let status = unsafe {
                CVMetalTextureCacheCreate(
                    null(),
                    null(),
                    Retained::as_ptr(&self.device) as *mut c_void,
                    null(),
                    &mut tex_cache,
                )
            };
            if status != 0 || tex_cache.is_null() {
                unsafe { CFRelease(pool) };
                bail!("CVMetalTextureCacheCreate failed with status {status}");
            }
            let pipeline = match unsafe { compile_compute_pipeline(&self.device) } {
                Ok(p) => p,
                Err(e) => {
                    unsafe {
                        CFRelease(pool);
                        CFRelease(tex_cache);
                    }
                    return Err(e);
                }
            };
            let mut slots = Vec::with_capacity(RING_SIZE);
            for _ in 0..RING_SIZE {
                let texture = unsafe { create_render_texture(&self.device, width, height)? };
                let tex_info =
                    unsafe { mtl::TextureInfo::new(Retained::as_ptr(&texture) as mtl::Handle) };
                let backend_rt =
                    gpu::backend_render_targets::make_mtl((width as i32, height as i32), &tex_info);
                let surface = gpu::surfaces::wrap_backend_render_target(
                    &mut self.context,
                    &backend_rt,
                    SurfaceOrigin::TopLeft,
                    ColorType::RGBA8888,
                    ColorSpace::new_srgb(),
                    None,
                )
                .context("wrap Metal render texture in Skia surface")?;
                slots.push(RingSlot {
                    texture,
                    surface,
                    last_cmd_buf: None,
                });
            }
            self.nv12 = Some(Nv12Converter {
                width,
                height,
                pool,
                tex_cache,
                pipeline,
                slots,
                next_slot: 0,
            });
            Ok(())
        }

        pub(super) fn render_gpu_frame<F>(
            &mut self,
            info: &ImageInfo,
            row_bytes: usize,
            pixels: Option<&mut Vec<u8>>,
            draw: F,
        ) -> Result<Option<MacosGpuFrame>>
        where
            F: FnOnce(&Canvas) -> Result<()>,
        {
            let width = info.width() as u32;
            let height = info.height() as u32;
            if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
                let Some(pixels) = pixels else {
                    let mut scratch = Vec::new();
                    self.render_into(info, row_bytes, &mut scratch, draw)?;
                    return Ok(None);
                };
                self.render_into(info, row_bytes, pixels, draw)?;
                return Ok(None);
            }
            autoreleasepool(|_| {
                self.ensure_nv12_converter(width, height)?;
                let nv12 = self.nv12.as_mut().expect("NV12 converter initialized");
                let slot_idx = nv12.next_slot;
                nv12.next_slot = (nv12.next_slot + 1) % RING_SIZE;
                let slot = &mut nv12.slots[slot_idx];
                if let Some(prev_cmd) = slot.last_cmd_buf.take() {
                    prev_cmd.waitUntilCompleted();
                }

                draw(slot.surface.canvas())?;
                if let Some(pixels) = pixels {
                    read_pixels_into(&mut slot.surface, info, row_bytes, pixels)?;
                } else {
                    self.context.flush_and_submit();
                }

                let mut pb: *mut c_void = null_mut();
                let status =
                    unsafe { CVPixelBufferPoolCreatePixelBuffer(null(), nv12.pool, &mut pb) };
                if status != 0 || pb.is_null() {
                    bail!("CVPixelBufferPoolCreatePixelBuffer failed with status {status}");
                }
                unsafe { record_pool_acquire(pb) };

                let mut cv_y: *mut c_void = null_mut();
                let mut cv_uv: *mut c_void = null_mut();
                let s1 = unsafe {
                    CVMetalTextureCacheCreateTextureFromImage(
                        null(),
                        nv12.tex_cache,
                        pb,
                        null(),
                        MTLPixelFormat::R8Unorm.0,
                        width as usize,
                        height as usize,
                        0,
                        &mut cv_y,
                    )
                };
                let s2 = unsafe {
                    CVMetalTextureCacheCreateTextureFromImage(
                        null(),
                        nv12.tex_cache,
                        pb,
                        null(),
                        MTLPixelFormat::RG8Unorm.0,
                        (width / 2) as usize,
                        (height / 2) as usize,
                        1,
                        &mut cv_uv,
                    )
                };
                if s1 != 0 || s2 != 0 || cv_y.is_null() || cv_uv.is_null() {
                    if !cv_y.is_null() {
                        unsafe { CFRelease(cv_y) };
                    }
                    if !cv_uv.is_null() {
                        unsafe { CFRelease(cv_uv) };
                    }
                    unsafe { release_tracked_pixel_buffer(pb) };
                    bail!("CVMetalTextureCacheCreateTextureFromImage failed ({s1}, {s2})");
                }

                let y_mtl = unsafe { CVMetalTextureGetTexture(cv_y) as *mut AnyObject };
                let uv_mtl = unsafe { CVMetalTextureGetTexture(cv_uv) as *mut AnyObject };
                let cmd_buf = self
                    .queue
                    .commandBuffer()
                    .context("create Metal command buffer for NV12 conversion")?;
                let comp: *mut AnyObject = unsafe { msg_send![&*cmd_buf, computeCommandEncoder] };
                unsafe {
                    let _: () = msg_send![comp, setComputePipelineState: &*nv12.pipeline];
                    let src_tex: *const AnyObject = Retained::as_ptr(&slot.texture);
                    let _: () = msg_send![comp, setTexture: src_tex, atIndex: 0usize];
                    let _: () = msg_send![comp, setTexture: y_mtl, atIndex: 1usize];
                    let _: () = msg_send![comp, setTexture: uv_mtl, atIndex: 2usize];
                    let tg_size = MTLSize {
                        width: 16,
                        height: 16,
                        depth: 1,
                    };
                    let tgs = MTLSize {
                        width: (width as usize).div_ceil(2).div_ceil(16),
                        height: (height as usize).div_ceil(2).div_ceil(16),
                        depth: 1,
                    };
                    let _: () = msg_send![
                        comp,
                        dispatchThreadgroups: tgs,
                        threadsPerThreadgroup: tg_size
                    ];
                    let _: () = msg_send![comp, endEncoding];
                }
                cmd_buf.commit();
                unsafe {
                    CFRelease(cv_y);
                    CFRelease(cv_uv);
                    CVMetalTextureCacheFlush(nv12.tex_cache, 0);
                }

                slot.last_cmd_buf = Some(cmd_buf.clone());
                let cmd_raw = Retained::into_raw(cmd_buf) as *mut c_void;
                Ok(Some(MacosGpuFrame {
                    pixel_buffer: pb,
                    cmd_buf: cmd_raw,
                }))
            })
        }
    }
}

#[cfg(target_os = "macos")]
use macos_metal::MetalBackend;
#[cfg(target_os = "macos")]
pub(crate) use macos_metal::{
    CFRelease, CVPixelBufferPoolCreatePixelBuffer, create_pixel_buffer_pool, record_pool_acquire,
    release_tracked_pixel_buffer,
};
#[cfg(target_os = "macos")]
pub use macos_metal::{
    MacosGpuFrame, MacosPoolTelemetry, macos_pool_telemetry, reset_macos_pool_telemetry,
};

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::{AlphaType, Color, ColorSpace, ColorType, ISize};

    #[test]
    fn raster_backend_draws_directly_into_output_buffer() -> Result<()> {
        let info = ImageInfo::new(
            ISize::new(2, 1),
            ColorType::RGBA8888,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let mut pixels = Vec::new();
        render_raster_into(&info, 8, &mut pixels, |canvas| {
            canvas.clear(Color::from_argb(255, 12, 34, 56));
            Ok(())
        })?;
        assert_eq!(pixels, [12, 34, 56, 255, 12, 34, 56, 255]);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_linux_backend_requests_without_initializing_vulkan() {
        use std::ffi::OsStr;

        assert_eq!(parse_backend_request(None).unwrap(), BackendRequest::Auto);
        assert_eq!(
            parse_backend_request(Some(OsStr::new("auto"))).unwrap(),
            BackendRequest::Auto
        );
        assert_eq!(
            parse_backend_request(Some(OsStr::new("vulkan"))).unwrap(),
            BackendRequest::Vulkan
        );
        assert_eq!(
            parse_backend_request(Some(OsStr::new(" VK "))).unwrap(),
            BackendRequest::Vulkan
        );
        assert_eq!(
            parse_backend_request(Some(OsStr::new("raster"))).unwrap(),
            BackendRequest::Raster
        );
        assert_eq!(
            parse_backend_request(Some(OsStr::new("cpu"))).unwrap(),
            BackendRequest::Raster
        );
        assert!(parse_backend_request(Some(OsStr::new("metal"))).is_err());
    }
}
