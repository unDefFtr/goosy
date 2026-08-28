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
struct MetalBackend {
    context: skia_safe::gpu::DirectContext,
    _backend: skia_safe::gpu::mtl::BackendContext,
    _queue: objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLCommandQueue>>,
    surface: Option<Surface>,
}

#[cfg(target_os = "macos")]
impl MetalBackend {
    fn new() -> Option<Self> {
        use objc2::{rc::Retained, runtime::ProtocolObject};
        use objc2_metal::{MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice};
        use skia_safe::gpu::{self, mtl};

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
        let queue: Retained<ProtocolObject<dyn MTLCommandQueue>> = queue;
        Some(Self {
            context,
            _backend: backend,
            _queue: queue,
            surface: None,
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
    }
}

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
