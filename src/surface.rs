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
#[cfg(target_os = "linux")]
const EXTERNAL_SLOT_COUNT: usize = 8;

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
    pub fn try_new_for_encoder(
        encoder: crate::EncoderBackend,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        #[cfg(target_os = "linux")]
        if matches!(encoder, crate::EncoderBackend::FfmpegNextNvencZeroCopy) {
            if requested_backend()? == BackendRequest::Raster {
                bail!(
                    "Vulkan/CUDA/NVENC zero-copy requires Vulkan; GOOSY_RENDER_BACKEND=raster was requested"
                );
            }
            return VulkanBackend::new_external(width, height)
                .map(|vulkan| Self {
                    backend: Backend::Vulkan(vulkan),
                })
                .context("negotiate Vulkan/CUDA/NVENC zero-copy capability");
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (encoder, width, height);
        Self::try_new()
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
            let odd_dimensions = info.width() % 2 != 0 || info.height() % 2 != 0;
            let gpu_frame = metal.render_gpu_frame(
                info,
                row_bytes,
                (read_cpu_pixels || odd_dimensions).then(|| frame.pixels_mut()),
                draw,
            )?;
            frame.set_gpu_frame(gpu_frame);
            return Ok(());
        }
        #[cfg(target_os = "windows")]
        if let Backend::Direct3D(direct3d) = &mut self.backend {
            let gpu_frame = direct3d.render_gpu_frame(
                info,
                row_bytes,
                frame.pixels_mut(),
                read_cpu_pixels,
                draw,
            )?;
            frame.set_win_gpu_frame(gpu_frame);
            return Ok(());
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = read_cpu_pixels;
        #[cfg(target_os = "linux")]
        {
            frame.clear_gpu_payload();
            self.render_into(info, row_bytes, frame.pixels_mut(), draw)?;
            if let Some(payload) = self.gpu_payload()? {
                frame.set_gpu_payload(payload);
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.render_into(info, row_bytes, frame.pixels_mut(), draw)
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn gpu_payload(
        &self,
    ) -> Result<Option<std::sync::Arc<crate::gpu_interop::GpuFramePayload>>> {
        match &self.backend {
            Backend::Vulkan(vulkan) if vulkan.external_mode => vulkan.gpu_payload().map(Some),
            #[cfg(target_os = "linux")]
            Backend::Vulkan(_) => Ok(None),
            _ => Ok(None),
        }
    }

    pub(crate) fn is_external_vulkan(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let Backend::Vulkan(vulkan) = &self.backend {
            return vulkan.external_mode;
        }
        false
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
    physical_device: ash::vk::PhysicalDevice,
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
struct ExternalVulkanSlot {
    image: ash::vk::Image,
    image_view: ash::vk::ImageView,
    image_memory: ash::vk::DeviceMemory,
    image_size: ash::vk::DeviceSize,
    nv12_buffer: ash::vk::Buffer,
    buffer_memory: ash::vk::DeviceMemory,
    buffer_size: ash::vk::DeviceSize,
    semaphore: ash::vk::Semaphore,
    cuda_mapping: std::sync::Arc<crate::gpu_interop::CudaNv12Mapping>,
    cuda_semaphore: std::sync::Arc<crate::gpu_interop::CudaExternalSemaphore>,
    token: std::sync::Arc<crate::gpu_interop::SlotToken>,
    layout: crate::gpu_interop::Nv12Layout,
    descriptor_set: ash::vk::DescriptorSet,
    command_buffer: ash::vk::CommandBuffer,
    fence: ash::vk::Fence,
    image_initialized: bool,
}

#[cfg(target_os = "linux")]
struct ComputeResources {
    command_pool: ash::vk::CommandPool,
    descriptor_pool: ash::vk::DescriptorPool,
    descriptor_set_layout: ash::vk::DescriptorSetLayout,
    pipeline_layout: ash::vk::PipelineLayout,
    pipeline: ash::vk::Pipeline,
    shader_module: ash::vk::ShaderModule,
}

#[cfg(target_os = "linux")]
struct VulkanBackend {
    surfaces: Vec<Option<Surface>>,
    context: Option<skia_safe::gpu::DirectContext>,
    external_slots: Vec<ExternalVulkanSlot>,
    compute: Option<ComputeResources>,
    native: VulkanNative,
    queue: ash::vk::Queue,
    queue_family_index: u32,
    device_uuid: [u8; 16],
    device_name: String,
    vendor_id: u32,
    device_type: ash::vk::PhysicalDeviceType,
    external_mode: bool,
    active_slot: Option<usize>,
}
#[cfg(target_os = "linux")]
impl VulkanBackend {
    fn initialize_compute(&mut self, _width: u32, _height: u32) -> Result<()> {
        use ash::vk;
        let device = self
            .native
            .device
            .as_ref()
            .context("Vulkan device unavailable")?;
        let shader = include_bytes!(env!("GOOSY_NV12_SHADER_PATH"));
        let shader = shader.as_slice();
        let words = ash::util::read_spv(&mut std::io::Cursor::new(shader))
            .context("parse compiled RGBA-to-NV12 shader")?;
        let shader_module = unsafe {
            device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        }
        .context("create RGBA-to-NV12 shader module")?;
        let bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
        ];
        let descriptor_set_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )
        }
        .context("create NV12 descriptor-set layout")?;
        let push_range = vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(24);
        let pipeline_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(std::slice::from_ref(&descriptor_set_layout))
                    .push_constant_ranges(std::slice::from_ref(&push_range)),
                None,
            )
        }
        .context("create NV12 pipeline layout")?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(shader_module)
            .name(c"main");
        let pipeline_info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(pipeline_layout);
        let pipeline = unsafe {
            device.create_compute_pipelines(
                vk::PipelineCache::null(),
                std::slice::from_ref(&pipeline_info),
                None,
            )
        }
        .map_err(|(_, error)| error)
        .context("create NV12 compute pipeline")?[0];
        let pool_sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(EXTERNAL_SLOT_COUNT as u32),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(EXTERNAL_SLOT_COUNT as u32),
        ];
        let descriptor_pool = unsafe {
            device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(EXTERNAL_SLOT_COUNT as u32)
                    .pool_sizes(&pool_sizes),
                None,
            )
        }
        .context("create NV12 descriptor pool")?;
        let layouts = [descriptor_set_layout; EXTERNAL_SLOT_COUNT];
        let sets = unsafe {
            device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(descriptor_pool)
                    .set_layouts(&layouts),
            )
        }
        .context("allocate NV12 descriptor sets")?;
        let command_pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(self.queue_family_index)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .context("create NV12 command pool")?;
        let command_buffers = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(EXTERNAL_SLOT_COUNT as u32),
            )
        }
        .context("allocate NV12 command buffers")?;
        for (index, slot) in self.external_slots.iter_mut().enumerate() {
            let image_info = vk::DescriptorImageInfo::default()
                .image_view(slot.image_view)
                .image_layout(vk::ImageLayout::GENERAL);
            let buffer_info = vk::DescriptorBufferInfo::default()
                .buffer(slot.nv12_buffer)
                .offset(0)
                .range(slot.buffer_size);
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(sets[index])
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(std::slice::from_ref(&image_info)),
                vk::WriteDescriptorSet::default()
                    .dst_set(sets[index])
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(std::slice::from_ref(&buffer_info)),
            ];
            unsafe {
                device.update_descriptor_sets(&writes, &[]);
            }
            slot.command_buffer = command_buffers[index];
            slot.descriptor_set = sets[index];
            slot.fence = unsafe {
                device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
            }
            .context("create NV12 slot fence")?;
        }
        self.compute = Some(ComputeResources {
            command_pool,
            descriptor_pool,
            descriptor_set_layout,
            pipeline_layout,
            pipeline,
            shader_module,
        });
        Ok(())
    }
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
            physical_device: vk::PhysicalDevice::null(),
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
            let mut id_properties = vk::PhysicalDeviceIDProperties::default();
            let mut properties2 =
                vk::PhysicalDeviceProperties2::default().push_next(&mut id_properties);
            unsafe { instance.get_physical_device_properties2(physical_device, &mut properties2) };
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
                id_properties.device_uuid,
            ));
        }
        candidates.sort_by_key(|candidate| candidate.0);
        let Some((_, physical_device, queue_family_index, properties, device_uuid)) =
            candidates.into_iter().next()
        else {
            bail!("no graphics-capable Vulkan 1.1 device");
        };
        native.physical_device = physical_device;

        let priorities = [1.0_f32];
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&priorities);
        let available_extensions =
            unsafe { instance.enumerate_device_extension_properties(physical_device) }
                .context("enumerate Vulkan device extensions")?;
        let extension_supported = |name: &std::ffi::CStr| {
            available_extensions.iter().any(|property| unsafe {
                std::ffi::CStr::from_ptr(property.extension_name.as_ptr()) == name
            })
        };
        let enabled_extensions = [
            vk::KHR_EXTERNAL_MEMORY_FD_NAME,
            vk::KHR_EXTERNAL_SEMAPHORE_FD_NAME,
        ]
        .into_iter()
        .filter(|name| extension_supported(name))
        .map(|name| name.as_ptr())
        .collect::<Vec<_>>();
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(std::slice::from_ref(&queue_info))
            .enabled_extension_names(&enabled_extensions);
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
            surfaces: vec![None],
            context: Some(context),
            external_slots: Vec::new(),
            compute: None,
            native,
            queue,
            queue_family_index,
            device_uuid,
            device_name,
            vendor_id: properties.vendor_id,
            device_type: properties.device_type,
            external_mode: false,
            active_slot: None,
        })
    }
    fn create_external_slot(&self, width: u32, height: u32) -> Result<ExternalVulkanSlot> {
        use ash::vk;
        use std::os::fd::{FromRawFd, OwnedFd};
        let layout = crate::gpu_interop::Nv12Layout::nvenc(width, height)?;
        let instance = self
            .native
            .instance
            .as_ref()
            .context("Vulkan instance unavailable")?;
        let device = self
            .native
            .device
            .as_ref()
            .context("Vulkan device unavailable")?;
        let memory_type = |bits: u32| -> Result<u32> {
            let properties = unsafe {
                instance.get_physical_device_memory_properties(self.native.physical_device)
            };
            (0..properties.memory_type_count)
                .find(|index| {
                    bits & (1 << index) != 0
                        && properties.memory_types[*index as usize]
                            .property_flags
                            .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("no device-local Vulkan memory type supports external sharing")
                })
        };

        let mut image_external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let image_info = vk::ImageCreateInfo::default()
            .push_next(&mut image_external)
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::STORAGE)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { device.create_image(&image_info, None) }
            .context("create exportable Vulkan RGBA image")?;
        let image_requirements = unsafe { device.get_image_memory_requirements(image) };
        let mut image_export = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let mut image_dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let image_allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(image_requirements.size)
            .memory_type_index(memory_type(image_requirements.memory_type_bits)?)
            .push_next(&mut image_export)
            .push_next(&mut image_dedicated);
        let image_memory = match unsafe { device.allocate_memory(&image_allocate, None) } {
            Ok(memory) => memory,
            Err(error) => {
                unsafe { device.destroy_image(image, None) };
                return Err(error).context("allocate exportable Vulkan RGBA memory");
            }
        };
        if let Err(error) = unsafe { device.bind_image_memory(image, image_memory, 0) } {
            unsafe {
                device.free_memory(image_memory, None);
                device.destroy_image(image, None);
            }
            return Err(error).context("bind exportable Vulkan RGBA memory");
        }

        let image_view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let image_view = match unsafe { device.create_image_view(&image_view_info, None) } {
            Ok(view) => view,
            Err(error) => {
                unsafe {
                    device.free_memory(image_memory, None);
                    device.destroy_image(image, None);
                }
                return Err(error).context("create Vulkan RGBA storage image view");
            }
        };
        let size = layout.size as vk::DeviceSize;
        let mut buffer_external = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let buffer_info = vk::BufferCreateInfo::default()
            .push_next(&mut buffer_external)
            .size(size)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let nv12_buffer = match unsafe { device.create_buffer(&buffer_info, None) } {
            Ok(buffer) => buffer,
            Err(error) => {
                unsafe {
                    device.free_memory(image_memory, None);
                    device.destroy_image(image, None);
                }
                return Err(error).context("create exportable Vulkan NV12 buffer");
            }
        };
        let buffer_requirements = unsafe { device.get_buffer_memory_requirements(nv12_buffer) };
        let mut buffer_export = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(nv12_buffer);
        let buffer_allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(buffer_requirements.size)
            .memory_type_index(memory_type(buffer_requirements.memory_type_bits)?)
            .push_next(&mut buffer_export)
            .push_next(&mut dedicated);
        let buffer_memory = match unsafe { device.allocate_memory(&buffer_allocate, None) } {
            Ok(memory) => memory,
            Err(error) => {
                unsafe {
                    device.destroy_buffer(nv12_buffer, None);
                    device.free_memory(image_memory, None);
                    device.destroy_image(image, None);
                }
                return Err(error).context("allocate exportable Vulkan NV12 memory");
            }
        };
        if let Err(error) = unsafe { device.bind_buffer_memory(nv12_buffer, buffer_memory, 0) } {
            unsafe {
                device.free_memory(buffer_memory, None);
                device.destroy_buffer(nv12_buffer, None);
                device.free_memory(image_memory, None);
                device.destroy_image(image, None);
            }
            return Err(error).context("bind exportable Vulkan NV12 memory");
        }

        let mut semaphore_export = vk::ExportSemaphoreCreateInfo::default()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
        let semaphore_info = vk::SemaphoreCreateInfo::default().push_next(&mut semaphore_export);
        let semaphore = match unsafe { device.create_semaphore(&semaphore_info, None) } {
            Ok(semaphore) => semaphore,
            Err(error) => {
                unsafe {
                    device.free_memory(buffer_memory, None);
                    device.destroy_buffer(nv12_buffer, None);
                    device.free_memory(image_memory, None);
                    device.destroy_image(image, None);
                }
                return Err(error).context("create exportable Vulkan semaphore");
            }
        };
        let memory_loader = ash::khr::external_memory_fd::Device::new(instance, device);
        let memory_info = vk::MemoryGetFdInfoKHR::default()
            .memory(buffer_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let mut memory_fd = -1;
        let result = unsafe {
            (memory_loader.fp().get_memory_fd_khr)(device.handle(), &memory_info, &mut memory_fd)
        };
        if result != vk::Result::SUCCESS || memory_fd < 0 {
            unsafe {
                device.destroy_semaphore(semaphore, None);
                device.free_memory(buffer_memory, None);
                device.destroy_buffer(nv12_buffer, None);
                device.free_memory(image_memory, None);
                device.destroy_image(image, None);
            }
            bail!("export Vulkan NV12 memory FD: {result:?}");
        }
        let semaphore_loader = ash::khr::external_semaphore_fd::Device::new(instance, device);
        let semaphore_info = vk::SemaphoreGetFdInfoKHR::default()
            .semaphore(semaphore)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
        let mut semaphore_fd = -1;
        let result = unsafe {
            (semaphore_loader.fp().get_semaphore_fd_khr)(
                device.handle(),
                &semaphore_info,
                &mut semaphore_fd,
            )
        };
        if result != vk::Result::SUCCESS || semaphore_fd < 0 {
            unsafe {
                device.destroy_semaphore(semaphore, None);
                device.free_memory(buffer_memory, None);
                device.destroy_buffer(nv12_buffer, None);
                device.free_memory(image_memory, None);
                device.destroy_image(image, None);
            }
            bail!("export Vulkan semaphore FD: {result:?}");
        }
        let layout = crate::gpu_interop::Nv12Layout::nvenc(width, height)?;
        let memory_fd = unsafe { OwnedFd::from_raw_fd(memory_fd) };
        let cuda_mapping = std::sync::Arc::new(crate::gpu_interop::import_cuda_nv12_memory(
            memory_fd,
            buffer_requirements.size as usize,
        )?);
        let cuda_semaphore =
            std::sync::Arc::new(crate::gpu_interop::import_cuda_external_semaphore(
                unsafe { OwnedFd::from_raw_fd(semaphore_fd) },
                cuda_mapping.context(),
            )?);
        Ok(ExternalVulkanSlot {
            image,
            image_view,
            image_memory,
            image_size: image_requirements.size,
            nv12_buffer,
            buffer_memory,
            buffer_size: buffer_requirements.size,
            semaphore,
            cuda_mapping,
            cuda_semaphore,
            token: std::sync::Arc::new(crate::gpu_interop::SlotToken::new()),
            layout,
            command_buffer: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            descriptor_set: vk::DescriptorSet::null(),
            image_initialized: false,
        })
    }

    fn new_external(width: u32, height: u32) -> Result<Self> {
        use ash::vk;
        let mut backend = Self::new()?;
        let instance = backend
            .native
            .instance
            .as_ref()
            .expect("Vulkan instance initialized");
        let extensions = unsafe {
            instance.enumerate_device_extension_properties(backend.native.physical_device)
        }
        .context("enumerate Vulkan device extensions for zero-copy")?;
        let has = |name: &std::ffi::CStr| {
            extensions.iter().any(|property| unsafe {
                std::ffi::CStr::from_ptr(property.extension_name.as_ptr()) == name
            })
        };
        let memory = has(vk::KHR_EXTERNAL_MEMORY_FD_NAME);
        let semaphore = has(vk::KHR_EXTERNAL_SEMAPHORE_FD_NAME);
        let cuda = crate::gpu_interop::cuda_driver_available();
        let nvenc = crate::gpu_interop::nvenc_available();
        let matching = crate::gpu_interop::cuda_device_uuid_matches(&backend.device_uuid);
        let shader_spirv = !include_bytes!(env!("GOOSY_NV12_SHADER_PATH")).is_empty();
        crate::gpu_interop::negotiate(crate::gpu_interop::CapabilityInputs {
            renderer_is_vulkan: true,
            external_memory_fd: memory,
            external_semaphore_fd: semaphore,
            cuda_driver: cuda,
            nvenc,
            matching_device: matching,
            shader_spirv,
        })?;
        eprintln!(
            "zero-copy capabilities negotiated: Vulkan external memory/semaphore, CUDA UUID, h264_nvenc, shader"
        );
        for _ in 0..EXTERNAL_SLOT_COUNT {
            let slot = backend.create_external_slot(width, height)?;
            backend.external_slots.push(slot);
        }
        backend.surfaces.resize_with(EXTERNAL_SLOT_COUNT, || None);
        backend.initialize_compute(width, height)?;
        eprintln!(
            "zero-copy Vulkan external slots allocated: {}",
            EXTERNAL_SLOT_COUNT
        );
        backend.external_mode = true;
        Ok(backend)
    }
    fn gpu_payload(&self) -> Result<std::sync::Arc<crate::gpu_interop::GpuFramePayload>> {
        let index = self.active_slot.context("no active Vulkan slot")?;
        let slot = self
            .external_slots
            .get(index)
            .context("active Vulkan slot disappeared")?;
        crate::gpu_interop::GpuFramePayload::new(
            slot.cuda_mapping.clone(),
            slot.cuda_semaphore.clone(),
            slot.token.clone(),
            slot.layout,
            index,
        )
        .map(std::sync::Arc::new)
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
        let slot_index = if self.external_mode {
            loop {
                if let Some(index) = self
                    .external_slots
                    .iter()
                    .position(|slot| slot.token.state() == crate::gpu_interop::SlotState::Free)
                {
                    break index;
                }
                std::thread::park_timeout(std::time::Duration::from_micros(50));
            }
        } else {
            0
        };
        self.active_slot = Some(slot_index);
        if self.external_mode {
            self.external_slots[slot_index].token.transition(
                crate::gpu_interop::SlotState::Free,
                crate::gpu_interop::SlotState::VulkanWriting,
            )?;
        }
        {
            let context = self.context.as_mut().expect("Vulkan context initialized");
            use ash::vk::Handle;
            if self
                .surfaces
                .get(slot_index)
                .and_then(Option::as_ref)
                .is_none()
            {
                if self.external_mode {
                    let slot = &self.external_slots[slot_index];
                    use skia_safe::gpu::backend_render_targets;
                    use skia_safe::gpu::vk as skia_vk;
                    let alloc = unsafe {
                        skia_vk::Alloc::from_device_memory(
                            slot.image_memory.as_raw() as _,
                            0,
                            slot.image_size,
                            skia_vk::AllocFlag::empty(),
                        )
                    };
                    let image_info = unsafe {
                        skia_vk::ImageInfo::new(
                            slot.image.as_raw() as _,
                            alloc,
                            skia_vk::ImageTiling::OPTIMAL,
                            skia_vk::ImageLayout::UNDEFINED,
                            skia_vk::Format::R8G8B8A8_UNORM,
                            1,
                            self.queue_family_index,
                            None,
                            None,
                            Some(skia_vk::SharingMode::EXCLUSIVE),
                        )
                    };
                    let target =
                        backend_render_targets::make_vk((info.width(), info.height()), &image_info);
                    self.surfaces[slot_index] = Some(
                        gpu::surfaces::wrap_backend_render_target(
                            context,
                            &target,
                            SurfaceOrigin::TopLeft,
                            skia_safe::ColorType::RGBA8888,
                            info.color_space(),
                            None,
                        )
                        .context("wrap exportable Vulkan image with Skia")?,
                    );
                } else {
                    self.surfaces[slot_index] = Some(
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
            }
            let surface = self.surfaces[slot_index]
                .as_mut()
                .expect("Vulkan surface initialized");
            draw(surface.canvas())?;
            context.flush_and_submit();
            if context.is_device_lost() {
                bail!("Vulkan GPU submission failed: device lost");
            }
            if context.oomed() {
                bail!("Vulkan GPU submission failed: out of memory");
            }
            if !self.external_mode {
                read_pixels_into(surface, info, row_bytes, pixels)
                    .context("read RGBA pixels from Vulkan surface")?;
            }
        }
        if self.external_mode {
            self.submit_compute(info.width() as u32, info.height() as u32, 0)?;
        }
        Ok(())
    }
    fn submit_compute(&mut self, width: u32, height: u32, frame_index: u64) -> Result<()> {
        use ash::vk;
        let device = self
            .native
            .device
            .as_ref()
            .context("Vulkan device unavailable")?;
        let compute = self
            .compute
            .as_ref()
            .context("Vulkan compute pipeline unavailable")?;
        let index = self.active_slot.context("no active Vulkan slot")?;
        let slot = &mut self.external_slots[index];
        unsafe {
            device
                .wait_for_fences(std::slice::from_ref(&slot.fence), true, u64::MAX)
                .context("wait zero-copy slot fence")?;
            device
                .reset_fences(std::slice::from_ref(&slot.fence))
                .context("reset zero-copy slot fence")?;
            device
                .reset_command_buffer(slot.command_buffer, vk::CommandBufferResetFlags::empty())
                .context("reset zero-copy command buffer")?;
            device
                .begin_command_buffer(slot.command_buffer, &vk::CommandBufferBeginInfo::default())
                .context("begin zero-copy compute command buffer")?;
            let old_image_layout = if slot.image_initialized {
                vk::ImageLayout::GENERAL
            } else {
                vk::ImageLayout::UNDEFINED
            };
            let image_barrier = vk::ImageMemoryBarrier::default()
                .src_access_mask(if slot.image_initialized {
                    vk::AccessFlags::SHADER_READ
                } else {
                    vk::AccessFlags::empty()
                })
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(old_image_layout)
                .new_layout(vk::ImageLayout::GENERAL)
                .image(slot.image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let buffer_barrier = vk::BufferMemoryBarrier::default()
                .src_access_mask(if slot.image_initialized {
                    vk::AccessFlags::SHADER_WRITE
                } else {
                    vk::AccessFlags::empty()
                })
                .dst_access_mask(vk::AccessFlags::SHADER_WRITE)
                .buffer(slot.nv12_buffer)
                .offset(0)
                .size(slot.buffer_size);
            device.cmd_pipeline_barrier(
                slot.command_buffer,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                std::slice::from_ref(&buffer_barrier),
                std::slice::from_ref(&image_barrier),
            );
            device.cmd_bind_pipeline(
                slot.command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                compute.pipeline,
            );
            device.cmd_bind_descriptor_sets(
                slot.command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                compute.pipeline_layout,
                0,
                std::slice::from_ref(&slot.descriptor_set),
                &[],
            );
            let params = [
                width,
                height,
                slot.layout.y_offset as u32,
                slot.layout.uv_offset as u32,
                slot.layout.y_pitch as u32,
                slot.layout.uv_pitch as u32,
            ];
            let bytes = std::slice::from_raw_parts(params.as_ptr() as *const u8, 24);
            device.cmd_push_constants(
                slot.command_buffer,
                compute.pipeline_layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                bytes,
            );
            device.cmd_dispatch(
                slot.command_buffer,
                width.div_ceil(32),
                height.div_ceil(32),
                1,
            );
            let post_barrier = vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .buffer(slot.nv12_buffer)
                .offset(0)
                .size(slot.buffer_size);
            device.cmd_pipeline_barrier(
                slot.command_buffer,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                std::slice::from_ref(&post_barrier),
                &[],
            );
            device
                .end_command_buffer(slot.command_buffer)
                .context("end zero-copy compute command buffer")?;
            let submit = vk::SubmitInfo::default()
                .command_buffers(std::slice::from_ref(&slot.command_buffer))
                .signal_semaphores(std::slice::from_ref(&slot.semaphore));
            device
                .queue_submit(self.queue, std::slice::from_ref(&submit), slot.fence)
                .with_context(|| {
                    format!("submit Vulkan NV12 compute frame {frame_index}, slot {index}")
                })?;
        }
        self.external_slots[index].image_initialized = true;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for VulkanBackend {
    fn drop(&mut self) {
        self.surfaces.clear();
        self.context.take();
        if let Some(device) = self.native.device.as_ref() {
            if let Some(compute) = self.compute.take() {
                unsafe {
                    device.destroy_pipeline(compute.pipeline, None);
                    device.destroy_pipeline_layout(compute.pipeline_layout, None);
                    device.destroy_shader_module(compute.shader_module, None);
                    device.destroy_descriptor_pool(compute.descriptor_pool, None);
                    device.destroy_descriptor_set_layout(compute.descriptor_set_layout, None);
                    device.destroy_command_pool(compute.command_pool, None);
                }
            }
            for slot in self.external_slots.drain(..) {
                let ExternalVulkanSlot {
                    image,
                    image_view,
                    image_memory,
                    nv12_buffer,
                    buffer_memory,
                    semaphore,
                    fence,
                    cuda_mapping,
                    cuda_semaphore,
                    ..
                } = slot;
                drop(cuda_semaphore);
                drop(cuda_mapping);
                unsafe {
                    if fence != ash::vk::Fence::null() {
                        device.destroy_fence(fence, None);
                    }
                    if semaphore != ash::vk::Semaphore::null() {
                        device.destroy_semaphore(semaphore, None);
                    }
                    device.destroy_image_view(image_view, None);
                    device.free_memory(buffer_memory, None);
                    device.destroy_buffer(nv12_buffer, None);
                    device.free_memory(image_memory, None);
                    device.destroy_image(image, None);
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
struct Direct3DBackend {
    // Rust drops fields in declaration order. Skia-wrapped surfaces inside
    // `nv12_ring` and `surface` MUST drop before `context`, `_backend`, and `device`.
    nv12_ring: Option<crate::win_qsv::D3d12SharedRgbaRing>,
    nv12_ring_failed: bool,
    // Keeps the companion D3D11 device alive in SharedD3d11Context's Weak registry
    // so QSV encoder initialization attaches to the exact same Intel adapter LUID.
    _shared_d3d11: Option<std::sync::Arc<crate::win_qsv::SharedD3d11Context>>,
    surface: Option<Surface>,
    context: skia_safe::gpu::DirectContext,
    _backend: skia_safe::gpu::d3d::BackendContext,
    queue: windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue,
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
        let queue: windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue =
            unsafe { selection.device.CreateCommandQueue(&queue_desc) }
                .context("create D3D12 direct command queue")?;
        // Pre-register and retain the companion D3D11 device on the exact same adapter LUID
        // when running on Intel graphics so QSV shares the identical D3D11 device.
        let shared_d3d11 = if selection.vendor_id == crate::win_qsv::d3d11_pipeline::INTEL_VENDOR_ID
        {
            let luid = unsafe { selection.device.GetAdapterLuid() };
            crate::win_qsv::SharedD3d11Context::get_or_create_for_luid(luid).ok()
        } else {
            None
        };
        let backend = skia_safe::gpu::d3d::BackendContext {
            adapter: selection.adapter,
            device: selection.device.clone(),
            queue: queue.clone(),
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
            nv12_ring: None,
            nv12_ring_failed: false,
            _shared_d3d11: shared_d3d11,
            surface: None,
            context,
            _backend: backend,
            queue,
            device: selection.device,
            adapter_name: selection.name,
            vendor_id: selection.vendor_id,
            dedicated_video_memory: selection.dedicated_video_memory,
        })
    }

    fn details(&self) -> String {
        let interop = self
            .nv12_ring
            .as_ref()
            .map(|r| format!(", D3D11 NV12 {}", r.converter.mode.label()))
            .unwrap_or_default();
        format!(
            "D3D12 / {} (vendor 0x{:04X}, {} MiB VRAM{})",
            self.adapter_name,
            self.vendor_id,
            self.dedicated_video_memory / (1024 * 1024),
            interop
        )
    }

    fn ensure_nv12_ring(&mut self, width: u32, height: u32) -> Result<()> {
        if self
            .nv12_ring
            .as_ref()
            .is_some_and(|r| r.width == width && r.height == height)
        {
            return Ok(());
        }
        self.nv12_ring = None;
        let ring = crate::win_qsv::D3d12SharedRgbaRing::new(
            &self.device,
            self.queue.clone(),
            &mut self.context,
            width,
            height,
        )?;
        self.nv12_ring = Some(ring);
        Ok(())
    }

    fn render_gpu_frame<F>(
        &mut self,
        info: &ImageInfo,
        row_bytes: usize,
        pixels: &mut Vec<u8>,
        read_cpu_pixels: bool,
        draw: F,
    ) -> Result<Option<crate::win_qsv::WinGpuFrame>>
    where
        F: FnOnce(&Canvas) -> Result<()>,
    {
        let width = info.width() as u32;
        let height = info.height() as u32;
        // Zero-copy QSV handoff requires Skia's D3D12 device to be on an Intel adapter
        // with even dimensions >= 64x64; otherwise populate CPU pixels cleanly for fallback.
        if self.vendor_id != crate::win_qsv::d3d11_pipeline::INTEL_VENDOR_ID
            || self.nv12_ring_failed
            || width < 64
            || height < 64
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
        {
            self.render_into(info, row_bytes, pixels, draw)?;
            return Ok(None);
        }
        if let Err(err) = self.ensure_nv12_ring(width, height) {
            self.nv12_ring_failed = true;
            eprintln!("goosy: D3D12->D3D11 NV12 interop unavailable; falling back to CPU readback: {err:#}");
            self.render_into(info, row_bytes, pixels, draw)?;
            return Ok(None);
        }
        let ring = self.nv12_ring.as_mut().expect("D3D12 NV12 ring initialized");
        let gpu_frame = ring.render_and_convert(
            &mut self.context,
            info,
            row_bytes,
            read_cpu_pixels.then_some(pixels),
            draw,
        )?;
        Ok(Some(gpu_frame))
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

        if self
            .surface
            .as_ref()
            .is_none_or(|s| s.image_info().dimensions() != info.dimensions())
        {
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
        msg_send,
        rc::{Retained, autoreleasepool},
        runtime::{AnyClass, AnyObject, ProtocolObject},
    };
    use objc2_metal::{
        MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLPixelFormat,
        MTLSize,
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

#[cfg(all(test, target_os = "linux"))]
#[test]
#[ignore = "requires NVIDIA Vulkan and CUDA driver"]
fn nvidia_external_slot_can_be_created() -> Result<()> {
    let mut backend = VulkanBackend::new_external(2, 2)?;
    let info = ImageInfo::new(
        skia_safe::ISize::new(2, 2),
        skia_safe::ColorType::RGBA8888,
        skia_safe::AlphaType::Premul,
        skia_safe::ColorSpace::new_srgb(),
    );
    let mut pixels = Vec::new();
    backend.render_into(&info, 8, &mut pixels, |canvas| {
        canvas.clear(skia_safe::Color::from_argb(255, 220, 30, 40));
        Ok(())
    })?;
    let payload = backend.gpu_payload()?;
    payload.wait_for_cuda(0)?;
    assert_eq!(backend.external_slots.len(), EXTERNAL_SLOT_COUNT);
    Ok(())
}
