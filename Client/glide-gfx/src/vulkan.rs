
use ash::{vk, Entry};
use ash::vk::Handle;
use std::{collections::HashMap, ffi::CString};
use x11::xlib;
use shaderc::ShaderKind;

// Swapchain setup stays here so the render path can stay pretty dumb (like lithium cuz he thinks hes better than me)

#[derive(Debug)]
pub enum VulkanError {
    Loader(ash::LoadingError),
    Vk(vk::Result),
    NoGraphicsDevice,
    NoMemoryType,
    BufferTooSmall,
    BufferNotFound,
    CommandBufferBusy,
    TextureNotFound,
    UnsupportedTextureFormat,
    TextureTooSmall,
    X11SurfaceUnavailable,
    SwapchainUnavailable,
}

impl std::fmt::Display for VulkanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Loader(error) => write!(f, "failed to load Vulkan: {error}"),
            Self::Vk(error) => write!(f, "Vulkan error: {error:?}"),
            Self::NoGraphicsDevice => write!(f, "no Vulkan physical device with graphics support"),
            Self::NoMemoryType => write!(f, "Vulkan has no compatible memory type"),
            Self::BufferTooSmall => write!(f, "buffer upload exceeds allocated buffer size"),
            Self::BufferNotFound => write!(f, "buffer resource does not exist"),
            Self::CommandBufferBusy => write!(f, "GPU command buffer is still in flight"),
            Self::TextureNotFound => write!(f, "texture resource does not exist"),
            Self::UnsupportedTextureFormat => write!(f, "texture format is unsupported by this Vulkan bootstrap path"),
            Self::TextureTooSmall => write!(f, "texture upload exceeds allocated texture size"),
            Self::X11SurfaceUnavailable => write!(f, "X11 Vulkan surface is unavailable"),
            Self::SwapchainUnavailable => write!(f, "Vulkan swapchain is unavailable"),
        }
    }
}

impl std::error::Error for VulkanError {}
impl From<vk::Result> for VulkanError {
    fn from(value: vk::Result) -> Self { Self::Vk(value) }
}

struct BufferResource {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: vk::DeviceSize,
    usage: super::BufferUsage,
    host_visible: bool,
}

struct TextureResource {
    image: vk::Image,
    memory: vk::DeviceMemory,
    width: u32,
    height: u32,
    bytes_per_pixel: usize,
    layout: vk::ImageLayout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PipelineKey {
    layout: super::VertexLayout,
    primitive: super::Primitive,
    blend: super::BlendMode,
    depth: bool,
}

pub struct VulkanBackend {
    _entry: Entry,
    instance: ash::Instance,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    command_pool: vk::CommandPool,
    command_buffers: Vec<vk::CommandBuffer>,
    current_frame: usize,
    frame_fences: Vec<vk::Fence>,
    acquire_semaphores: Vec<vk::Semaphore>,
    upload_command_buffer: vk::CommandBuffer,
    upload_recording: bool,
    pending_upload_staging: Vec<(vk::Buffer, vk::DeviceMemory, vk::DeviceSize)>,
    deferred_upload_staging: Vec<Vec<(vk::Buffer, vk::DeviceMemory, vk::DeviceSize)>>,
    staging_pool: Vec<(vk::Buffer, vk::DeviceMemory, vk::DeviceSize)>,
    surface_loader: ash::khr::surface::Instance,
    xlib_surface_loader: ash::khr::xlib_surface::Instance,
    swapchain_loader: Option<ash::khr::swapchain::Device>,
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    swapchain_views: Vec<vk::ImageView>,
    swapchain_framebuffers: Vec<vk::Framebuffer>,
    swapchain_format: vk::Format,
    swapchain_extent: vk::Extent2D,
    image_available: Option<vk::Semaphore>,
    render_finished: Option<vk::Semaphore>,
    present_semaphores: Vec<vk::Semaphore>,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    pipeline_cache: vk::PipelineCache,
    vert_module: vk::ShaderModule,
    frag_module: vk::ShaderModule,
    pipelines: HashMap<PipelineKey, vk::Pipeline>,
    buffers: HashMap<super::ResourceId, BufferResource>,
    textures: HashMap<super::ResourceId, TextureResource>,
    render_image: vk::Image,
    render_memory: vk::DeviceMemory,
    render_view: vk::ImageView,
    render_framebuffer: vk::Framebuffer,
    render_width: u32,
    render_height: u32,
    render_layout: vk::ImageLayout,
    readback_buffer: vk::Buffer,
    readback_memory: vk::DeviceMemory,
    readback_size: vk::DeviceSize,
}

impl VulkanBackend {
    pub unsafe fn new() -> Result<Self, VulkanError> {
        let entry = Entry::load().map_err(VulkanError::Loader)?;
        let app_name = CString::new("Glide").unwrap();
        let engine_name = CString::new("Glide Graphics").unwrap();
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .engine_name(&engine_name)
            .api_version(vk::API_VERSION_1_0);
        let extension_names = [ash::khr::surface::NAME.as_ptr(), ash::khr::xlib_surface::NAME.as_ptr()];
        let instance = entry.create_instance(
            &vk::InstanceCreateInfo::default().application_info(&app_info)
                .enabled_extension_names(&extension_names), None,
        )?;

        let physical_devices = instance.enumerate_physical_devices()?;
        let mut selected: Option<(vk::PhysicalDevice, u32, i32)> = None;
        for physical_device in physical_devices {
            let properties = instance.get_physical_device_properties(physical_device);
            let rank = match properties.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => 300,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 200,
                vk::PhysicalDeviceType::VIRTUAL_GPU => 150,
                vk::PhysicalDeviceType::CPU => 10,
                _ => 100,
            };
            if let Some((index, _)) = instance
                .get_physical_device_queue_family_properties(physical_device)
                .iter()
                .enumerate()
                .find(|(_, family)| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            {
                if selected.map(|(_, _, old_rank)| rank > old_rank).unwrap_or(true) {
                    selected = Some((physical_device, index as u32, rank));
                }
            }
        }
        let (physical_device, queue_family, _) =
            selected.ok_or(VulkanError::NoGraphicsDevice)?;

        let priority = [1.0_f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family).queue_priorities(&priority)];
        let device_extensions = [ash::khr::swapchain::NAME.as_ptr()];
        let device = instance.create_device(
            physical_device,
            &vk::DeviceCreateInfo::default()
                .queue_create_infos(&queue_info)
                .enabled_extension_names(&device_extensions), None,
        )?;
        let queue = device.get_device_queue(queue_family, 0);
        let memory_properties = instance.get_physical_device_memory_properties(physical_device);
        let command_pool = device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None,
        )?;
        const FRAMES_IN_FLIGHT: u32 = 3;
        let command_buffers = device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(FRAMES_IN_FLIGHT),
        )?;
        let render_command_buffers = command_buffers;
        let upload_command_buffer = device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        let frame_fences = (0..FRAMES_IN_FLIGHT)
            .map(|_| device.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None))
            .collect::<Result<Vec<_>, _>>()?;
        let acquire_semaphores = (0..FRAMES_IN_FLIGHT)
            .map(|_| device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None))
            .collect::<Result<Vec<_>, _>>()?;
        let pipeline_cache_data = std::env::var("GLIDE_PIPELINE_CACHE")
            .ok()
            .and_then(|path| std::fs::read(path).ok());
        let pipeline_cache_info = if let Some(data) = pipeline_cache_data.as_ref() {
            vk::PipelineCacheCreateInfo::default().initial_data(data)
        } else {
            vk::PipelineCacheCreateInfo::default()
        };
        let pipeline_cache = device.create_pipeline_cache(&pipeline_cache_info, None)?;

        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
        let xlib_surface_loader = ash::khr::xlib_surface::Instance::new(&entry, &instance);
        let format = vk::Format::R8G8B8A8_UNORM;
        let attachment = vk::AttachmentDescription::default()
            .format(format).samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR).store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let color_ref = [vk::AttachmentReference::default()
            .attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref);
        let render_pass = device.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(std::slice::from_ref(&attachment))
                .subpasses(std::slice::from_ref(&subpass)), None
        )?;
        let compiler = shaderc::Compiler::new()
            .ok_or_else(|| VulkanError::Vk(vk::Result::ERROR_INITIALIZATION_FAILED))?;
        let vertex_source = r#"#version 450
layout(location = 0) in vec3 in_pos;
layout(location = 0) out vec4 out_color;
void main() {
    gl_Position = vec4(in_pos, 1.0);
    out_color = vec4(1.0);
}"#;
        let fragment_source = r#"#version 450
layout(location = 0) in vec4 in_color;
layout(location = 0) out vec4 out_color;
void main() {
    out_color = in_color;
}"#;
        let vert = compiler.compile_into_spirv(vertex_source, ShaderKind::Vertex, "glide_capture.vert", "main", None)
            .map_err(|_| VulkanError::Vk(vk::Result::ERROR_INVALID_SHADER_NV))?;
        let frag = compiler.compile_into_spirv(fragment_source, ShaderKind::Fragment, "glide_capture.frag", "main", None)
            .map_err(|_| VulkanError::Vk(vk::Result::ERROR_INVALID_SHADER_NV))?;
        let vert_module = device.create_shader_module(
            &vk::ShaderModuleCreateInfo::default().code(vert.as_binary()), None
        )?;
        let frag_module = device.create_shader_module(
            &vk::ShaderModuleCreateInfo::default().code(frag.as_binary()), None
        )?;
        let pipeline_layout = device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default(), None
        )?;
        Ok(Self {
            _entry: entry, instance, physical_device, device, queue, queue_family,
            command_pool, command_buffers: render_command_buffers, current_frame: 0, frame_fences, acquire_semaphores,
            upload_command_buffer, upload_recording: false, pending_upload_staging: Vec::new(),
            deferred_upload_staging: (0..FRAMES_IN_FLIGHT).map(|_| Vec::new()).collect(),
            staging_pool: Vec::new(),
            surface_loader, xlib_surface_loader, swapchain_loader: None,
            surface: vk::SurfaceKHR::null(), swapchain: vk::SwapchainKHR::null(),
            swapchain_images: Vec::new(), swapchain_views: Vec::new(), swapchain_framebuffers: Vec::new(),
            swapchain_format: vk::Format::UNDEFINED,
            swapchain_extent: vk::Extent2D { width: 0, height: 0 },
            image_available: None, render_finished: None, present_semaphores: Vec::new(),
            memory_properties, render_pass, pipeline_layout, pipeline_cache, vert_module, frag_module,
            pipelines: HashMap::new(), buffers: HashMap::new(), textures: HashMap::new(),
            render_image: vk::Image::null(), render_memory: vk::DeviceMemory::null(),
            render_view: vk::ImageView::null(), render_framebuffer: vk::Framebuffer::null(),
            render_width: 0, render_height: 0, render_layout: vk::ImageLayout::UNDEFINED,
            readback_buffer: vk::Buffer::null(), readback_memory: vk::DeviceMemory::null(),
            readback_size: 0,
        })
    }

    pub fn physical_device(&self) -> vk::PhysicalDevice { self.physical_device }
    pub fn queue_family(&self) -> u32 { self.queue_family }
    pub fn queue(&self) -> vk::Queue { self.queue }

    pub unsafe fn device_name(&self) -> String {
        let properties = self.instance.get_physical_device_properties(self.physical_device);
        let bytes = properties.device_name.iter().take_while(|&&b| b != 0)
            .map(|&b| b as u8).collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub unsafe fn device_type(&self) -> vk::PhysicalDeviceType {
        self.instance.get_physical_device_properties(self.physical_device).device_type
    }

    fn memory_type_index(&self, requirements: vk::MemoryRequirements, flags: vk::MemoryPropertyFlags)
        -> Result<u32, VulkanError>
    {
        for index in 0..self.memory_properties.memory_type_count {
            if (requirements.memory_type_bits & (1 << index)) != 0
                && self.memory_properties.memory_types[index as usize].property_flags.contains(flags)
            {
                return Ok(index);
            }
        }
        Err(VulkanError::NoMemoryType)
    }

    unsafe fn create_buffer(
        &mut self, id: super::ResourceId, usage: super::BufferUsage, size: usize,
    ) -> Result<(), VulkanError> {
        let requested_size = size.max(1) as vk::DeviceSize;

        if let Some(existing) = self.buffers.get(&id) {
            if existing.usage == usage && existing.size >= requested_size {
                return Ok(());
            }
        }

        if let Some(old) = self.buffers.remove(&id) {
            self.device.destroy_buffer(old.buffer, None);
            self.device.free_memory(old.memory, None);
        }
        let usage_flags = match usage {
            super::BufferUsage::Vertex => vk::BufferUsageFlags::VERTEX_BUFFER,
            super::BufferUsage::Index => vk::BufferUsageFlags::INDEX_BUFFER,
            super::BufferUsage::Uniform => vk::BufferUsageFlags::UNIFORM_BUFFER,
            super::BufferUsage::Storage => vk::BufferUsageFlags::STORAGE_BUFFER,
        } | vk::BufferUsageFlags::TRANSFER_DST;
        let size = requested_size;
        let buffer = self.device.create_buffer(
            &vk::BufferCreateInfo::default().size(size).usage(usage_flags)
                .sharing_mode(vk::SharingMode::EXCLUSIVE), None,
        )?;
        let requirements = self.device.get_buffer_memory_requirements(buffer);
        let (memory_type, host_visible) = match self.memory_type_index(
            requirements, vk::MemoryPropertyFlags::DEVICE_LOCAL
        ) {
            Ok(index) => (index, false),
            Err(_) => {
                let host_flags = vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT;
                (self.memory_type_index(requirements, host_flags)?, true)
            }
        };
        let memory = match self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size).memory_type_index(memory_type), None,
        ) {
            Ok(memory) => memory,
            Err(error) => {
                self.device.destroy_buffer(buffer, None);
                return Err(error.into());
            }
        };
        if let Err(error) = self.device.bind_buffer_memory(buffer, memory, 0) {
            self.device.destroy_buffer(buffer, None);
            self.device.free_memory(memory, None);
            return Err(error.into());
        }
        self.buffers.insert(id, BufferResource { buffer, memory, size, usage, host_visible });
        Ok(())
    }

    unsafe fn begin_upload_batch(&mut self) -> Result<(), VulkanError> {
        if self.upload_recording {
            return Ok(());
        }
        self.device.reset_command_buffer(
            self.upload_command_buffer,
            vk::CommandBufferResetFlags::empty(),
        )?;
        self.device.begin_command_buffer(
            self.upload_command_buffer,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        self.upload_recording = true;
        Ok(())
    }

    unsafe fn acquire_staging_buffer(
        &mut self,
        size: vk::DeviceSize,
    ) -> Result<(vk::Buffer, vk::DeviceMemory, vk::DeviceSize), VulkanError> {
        if let Some(index) = self.staging_pool.iter().position(|(_, _, capacity)| *capacity >= size) {
            return Ok(self.staging_pool.swap_remove(index));
        }

        let staging = self.device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size.max(1))
                .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE), None,
        )?;
        let requirements = self.device.get_buffer_memory_requirements(staging);
        let memory_type = match self.memory_type_index(
            requirements,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ) {
            Ok(index) => index,
            Err(error) => {
                self.device.destroy_buffer(staging, None);
                return Err(error);
            }
        };
        let staging_memory = match self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type), None,
        ) {
            Ok(memory) => memory,
            Err(error) => {
                self.device.destroy_buffer(staging, None);
                return Err(error.into());
            }
        };
        if let Err(error) = self.device.bind_buffer_memory(staging, staging_memory, 0) {
            self.device.destroy_buffer(staging, None);
            self.device.free_memory(staging_memory, None);
            return Err(error.into());
        }
        Ok((staging, staging_memory, requirements.size))
    }

    unsafe fn upload_buffer(
        &mut self, id: super::ResourceId, offset: usize, data: &[u8],
    ) -> Result<(), VulkanError> {
        let (buffer, size, host_visible, memory) = {
            let resource = self.buffers.get(&id).ok_or(VulkanError::BufferNotFound)?;
            (resource.buffer, resource.size, resource.host_visible, resource.memory)
        };
        let end = (offset as vk::DeviceSize).saturating_add(data.len() as vk::DeviceSize);
        if end > size { return Err(VulkanError::BufferTooSmall); }
        if data.is_empty() { return Ok(()); }
        if host_visible {
            let mapped = self.device.map_memory(
                memory, offset as u64, data.len() as u64, vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast::<u8>(), data.len());
            self.device.unmap_memory(memory);
            return Ok(());
        }

        let (staging, staging_memory, staging_capacity) = self.acquire_staging_buffer(data.len() as vk::DeviceSize)?;
        let mapped = match self.device.map_memory(
            staging_memory, 0, data.len() as u64, vk::MemoryMapFlags::empty(),
        ) {
            Ok(mapped) => mapped,
            Err(error) => {
                self.device.destroy_buffer(staging, None);
                self.device.free_memory(staging_memory, None);
                return Err(error.into());
            }
        };
        std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast::<u8>(), data.len());
        self.device.unmap_memory(staging_memory);

        self.begin_upload_batch()?;
        self.device.cmd_copy_buffer(
            self.upload_command_buffer,
            staging,
            buffer,
            &[vk::BufferCopy::default()
                .src_offset(0)
                .dst_offset(offset as u64)
                .size(data.len() as u64)],
        );
        self.pending_upload_staging.push((staging, staging_memory, staging_capacity));
        Ok(())
    }

    unsafe fn flush_upload_batch(&mut self) -> Result<(), VulkanError> {
        if !self.upload_recording {
            return Ok(());
        }

        self.device.end_command_buffer(self.upload_command_buffer)?;
        let command_buffers = [self.upload_command_buffer];
        let submit = self.device.queue_submit(
            self.queue,
            &[vk::SubmitInfo::default().command_buffers(&command_buffers)],
            vk::Fence::null(),
        );
        if let Err(error) = submit {
            self.upload_recording = false;
            for (buffer, memory, _) in self.pending_upload_staging.drain(..) {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
            }
            return Err(error.into());
        }

        let staging = std::mem::take(&mut self.pending_upload_staging);
        self.deferred_upload_staging[self.current_frame].extend(staging);
        self.upload_recording = false;
        Ok(())
    }

    unsafe fn create_texture(
        &mut self, id: super::ResourceId, width: u32, height: u32,
        format: super::TextureFormat,
    ) -> Result<(), VulkanError> {
        if let Some(old) = self.textures.remove(&id) {
            self.device.destroy_image(old.image, None);
            self.device.free_memory(old.memory, None);
        }
        let (vk_format, bytes_per_pixel) = match format {
            super::TextureFormat::Rgba8 => (vk::Format::R8G8B8A8_UNORM, 4),
            super::TextureFormat::Bgra8 => (vk::Format::B8G8R8A8_UNORM, 4),
            super::TextureFormat::Rgb8 => (vk::Format::R8G8B8_UNORM, 3),
            super::TextureFormat::Depth24Stencil8 => return Err(VulkanError::UnsupportedTextureFormat),
        };

        let image = self.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D).format(vk_format)
                .extent(vk::Extent3D { width: width.max(1), height: height.max(1), depth: 1 })
                .mip_levels(1).array_layers(1).samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
                .initial_layout(vk::ImageLayout::UNDEFINED), None,
        )?;
        let requirements = self.device.get_image_memory_requirements(image);
        let memory_type = self.memory_type_index(
            requirements, vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let memory = match self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size).memory_type_index(memory_type), None,
        ) {
            Ok(memory) => memory,
            Err(error) => {
                self.device.destroy_image(image, None);
                return Err(error.into());
            }
        };
        if let Err(error) = self.device.bind_image_memory(image, memory, 0) {
            self.device.destroy_image(image, None);
            self.device.free_memory(memory, None);
            return Err(error.into());
        }
        self.textures.insert(id, TextureResource {
            image, memory, width: width.max(1), height: height.max(1), bytes_per_pixel,
            layout: vk::ImageLayout::UNDEFINED,
        });
        Ok(())
    }

    unsafe fn upload_texture(
        &mut self, id: super::ResourceId, x: u32, y: u32,
        width: u32, height: u32, data: &[u8],
    ) -> Result<(), VulkanError> {
        let (image, old_layout, texture_width, texture_height, bytes_per_pixel) = {
            let texture = self.textures.get(&id).ok_or(VulkanError::TextureNotFound)?;
            (texture.image, texture.layout, texture.width, texture.height, texture.bytes_per_pixel)
        };
        let expected = width as usize * height as usize * bytes_per_pixel;
        if data.len() < expected || x.saturating_add(width) > texture_width
            || y.saturating_add(height) > texture_height {
            return Err(VulkanError::TextureTooSmall);
        }
        if data.is_empty() {
            return Ok(());
        }

        self.begin_upload_batch()?;
        let (staging, staging_memory, staging_capacity) = self.acquire_staging_buffer(expected as vk::DeviceSize)?;
        let mapped = match self.device.map_memory(
            staging_memory, 0, expected as u64, vk::MemoryMapFlags::empty(),
        ) {
            Ok(mapped) => mapped,
            Err(error) => {
                self.device.destroy_buffer(staging, None);
                self.device.free_memory(staging_memory, None);
                return Err(error.into());
            }
        };
        std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast::<u8>(), expected);
        self.device.unmap_memory(staging_memory);

        let old_access = if old_layout == vk::ImageLayout::UNDEFINED {
            vk::AccessFlags::empty()
        } else {
            vk::AccessFlags::SHADER_READ
        };
        let old_stage = if old_layout == vk::ImageLayout::UNDEFINED {
            vk::PipelineStageFlags::TOP_OF_PIPE
        } else {
            vk::PipelineStageFlags::FRAGMENT_SHADER
        };
        self.device.cmd_pipeline_barrier(
            self.upload_command_buffer,
            old_stage,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[vk::ImageMemoryBarrier::default()
                .image(image)
                .src_access_mask(old_access)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(old_layout)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .subresource_range(vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1).layer_count(1))],
        );
        self.device.cmd_copy_buffer_to_image(
            self.upload_command_buffer,
            staging,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[vk::BufferImageCopy::default()
                .buffer_offset(0)
                .buffer_row_length(width)
                .buffer_image_height(height)
                .image_subresource(vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0).base_array_layer(0).layer_count(1))
                .image_offset(vk::Offset3D { x: x as i32, y: y as i32, z: 0 })
                .image_extent(vk::Extent3D { width, height, depth: 1 })],
        );
        self.device.cmd_pipeline_barrier(
            self.upload_command_buffer,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[vk::ImageMemoryBarrier::default()
                .image(image)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .subresource_range(vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1).layer_count(1))],
        );

        if let Some(texture) = self.textures.get_mut(&id) {
            texture.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        }
        self.pending_upload_staging.push((staging, staging_memory, staging_capacity));
        Ok(())
    }

    pub unsafe fn render_test_frame(&mut self, width: u32, height: u32) -> Result<(), VulkanError> {
        let format = vk::Format::R8G8B8A8_UNORM;
        let image = self.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D).format(format)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1).array_layers(1).samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                .initial_layout(vk::ImageLayout::UNDEFINED), None,
        )?;
        let requirements = self.device.get_image_memory_requirements(image);
        let memory_type = self.memory_type_index(
            requirements, vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let memory = match self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size).memory_type_index(memory_type), None,
        ) {
            Ok(memory) => memory,
            Err(error) => {
                self.device.destroy_image(image, None);
                return Err(error.into());
            }
        };
        self.device.bind_image_memory(image, memory, 0)?;

        let view = self.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image).view_type(vk::ImageViewType::TYPE_2D).format(format)
                .subresource_range(vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0).level_count(1).base_array_layer(0).layer_count(1)),
            None,
        )?;
        let attachment = vk::AttachmentDescription::default()
            .format(format).samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR).store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let color_ref = [vk::AttachmentReference::default()
            .attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref);
        let render_pass = self.device.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(std::slice::from_ref(&attachment))
                .subpasses(std::slice::from_ref(&subpass)), None,
        )?;
        let framebuffer = self.device.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(render_pass).attachments(std::slice::from_ref(&view))
                .width(width).height(height).layers(1), None,
        )?;

        let command_buffer = self.command_buffers[0];
        self.device.reset_command_buffer(
            command_buffer, vk::CommandBufferResetFlags::empty()
        )?;
        self.device.begin_command_buffer(
            command_buffer,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        let clear = [vk::ClearValue {
            color: vk::ClearColorValue { float32: [0.08, 0.16, 0.24, 1.0] },
        }];
        self.device.cmd_begin_render_pass(
            command_buffer,
            &vk::RenderPassBeginInfo::default()
                .render_pass(render_pass).framebuffer(framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D { width, height },
                })
                .clear_values(&clear),
            vk::SubpassContents::INLINE,
        );
        self.device.cmd_end_render_pass(command_buffer);
        self.device.end_command_buffer(command_buffer)?;
        let command_buffers = [command_buffer];
        self.device.queue_submit(
            self.queue,
            &[vk::SubmitInfo::default().command_buffers(&command_buffers)],
            vk::Fence::null(),
        )?;
        self.device.queue_wait_idle(self.queue)?;

        self.device.destroy_framebuffer(framebuffer, None);
        self.device.destroy_render_pass(render_pass, None);
        self.device.destroy_image_view(view, None);
        self.device.destroy_image(image, None);
        self.device.free_memory(memory, None);
        Ok(())
    }

    pub unsafe fn attach_x11(&mut self, display: *mut xlib::Display, window: u64, width: u32, height: u32) -> Result<(), VulkanError> {
        if display.is_null() || window == 0 { return Err(VulkanError::X11SurfaceUnavailable); }
        if !self.surface.is_null() { return Ok(()); }
        let info = vk::XlibSurfaceCreateInfoKHR::default().dpy(display.cast()).window(window as xlib::Window);
        self.surface = self.xlib_surface_loader.create_xlib_surface(&info, None).map_err(|_| VulkanError::X11SurfaceUnavailable)?;
        let formats = self.surface_loader.get_physical_device_surface_formats(self.physical_device, self.surface)?;
        let caps = self.surface_loader.get_physical_device_surface_capabilities(self.physical_device, self.surface)?;
        let modes = self.surface_loader.get_physical_device_surface_present_modes(self.physical_device, self.surface)?;
        if formats.is_empty() || modes.is_empty() { return Err(VulkanError::SwapchainUnavailable); }
        let format = formats.iter().copied().find(|f| f.format == vk::Format::R8G8B8A8_UNORM).unwrap_or_else(|| formats.iter().copied().find(|f| f.format == vk::Format::B8G8R8A8_UNORM).unwrap_or(formats[0]));
        let present_mode = if modes.contains(&vk::PresentModeKHR::IMMEDIATE) {
            vk::PresentModeKHR::IMMEDIATE
        } else if modes.contains(&vk::PresentModeKHR::MAILBOX) {
            vk::PresentModeKHR::MAILBOX
        } else {
            vk::PresentModeKHR::FIFO
        };
        let extent = if caps.current_extent.width != u32::MAX { caps.current_extent } else {
            vk::Extent2D { width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height) }
        };
        let desired = caps.min_image_count.saturating_add(1).max(3);
        let count = if caps.max_image_count > 0 { desired.min(caps.max_image_count) } else { desired };
        let loader = ash::khr::swapchain::Device::new(&self.instance, &self.device);
        let create = vk::SwapchainCreateInfoKHR::default().surface(self.surface).min_image_count(count)
            .image_format(format.format).image_color_space(format.color_space).image_extent(extent)
            .image_array_layers(1).image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE).pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE).present_mode(present_mode).clipped(true);
        self.swapchain = loader.create_swapchain(&create, None)?;
        self.swapchain_images = loader.get_swapchain_images(self.swapchain)?;
        self.swapchain_views = self.swapchain_images.iter().map(|&image| {
            self.device.create_image_view(&vk::ImageViewCreateInfo::default().image(image)
                .view_type(vk::ImageViewType::TYPE_2D).format(format.format)
                .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1).layer_count(1)), None)
        }).collect::<Result<Vec<_>, _>>()?;
        self.swapchain_framebuffers = self.swapchain_views.iter().map(|&view| {
            self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.render_pass)
                    .attachments(std::slice::from_ref(&view))
                    .width(extent.width.max(1)).height(extent.height.max(1)).layers(1),
                None,
            )
        }).collect::<Result<Vec<_>, _>>()?;
        self.swapchain_format = format.format;
        self.swapchain_extent = extent;
        self.image_available = None;
        self.render_finished = None;
        let mut present_semaphores = Vec::with_capacity(self.swapchain_images.len());
        for _ in 0..self.swapchain_images.len() {
            present_semaphores.push(self.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
        }
        self.render_finished = present_semaphores.first().copied();
        self.present_semaphores = present_semaphores;
        self.swapchain_loader = Some(loader);
        eprintln!("GLIDE_VULKAN_PRESENT=ready extent={}x{} images={}", extent.width, extent.height, self.swapchain_images.len());
        Ok(())
    }

    pub unsafe fn render_presented_frame(&mut self, commands: &[super::Command], width: u32, height: u32) -> Result<u32, VulkanError> {
        let wait_ns = if std::env::var("GLIDE_LOW_LATENCY").ok().as_deref() == Some("1") {
            250_000
        } else {
            1_000_000
        };
        let frame = (0..self.frame_fences.len())
            .map(|offset| (self.current_frame + offset) % self.frame_fences.len())
            .find(|&index| {
                self.device.wait_for_fences(
                    &[self.frame_fences[index]], true, wait_ns
                ).is_ok()
            });

        let Some(frame) = frame else {
            return Ok(0);
        };

        let frame_fence = self.frame_fences[frame];
        let image_available = self.acquire_semaphores[frame];
        let acquire = self.swapchain_loader.as_ref()
            .ok_or(VulkanError::SwapchainUnavailable)?
            .acquire_next_image(self.swapchain, wait_ns, image_available, vk::Fence::null());

        let (image_index, _) = match acquire {
            Ok(value) => value,
            Err(vk::Result::NOT_READY) | Err(vk::Result::TIMEOUT) => {
                return Ok(0);
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                return Ok(0);
            }
            Err(error) => {
                return Err(error.into());
            }
        };

        self.device.reset_fences(&[frame_fence])?;

        let command_buffer = self.command_buffers[frame];
        let (draws, _) = self.render_command_frame(commands, width, height, Some((
            self.swapchain_images[image_index as usize], image_index,
        )), command_buffer, frame_fence, image_available)?;
        self.current_frame = (frame + 1) % self.command_buffers.len();
        Ok(draws)
    }

    unsafe fn ensure_render_targets(&mut self, width: u32, height: u32) -> Result<(), VulkanError> {
        let width = width.max(1);
        let height = height.max(1);
        let readback_size = (width as vk::DeviceSize)
            .saturating_mul(height as vk::DeviceSize)
            .saturating_mul(4);

        if self.render_width == width && self.render_height == height
            && !self.render_image.is_null() && !self.readback_buffer.is_null()
        {
            return Ok(());
        }

        if !self.render_framebuffer.is_null() {
            self.device.destroy_framebuffer(self.render_framebuffer, None);
            self.render_framebuffer = vk::Framebuffer::null();
        }
        if !self.render_view.is_null() {
            self.device.destroy_image_view(self.render_view, None);
            self.render_view = vk::ImageView::null();
        }
        if !self.render_image.is_null() {
            self.device.destroy_image(self.render_image, None);
            self.render_image = vk::Image::null();
        }
        if !self.render_memory.is_null() {
            self.device.free_memory(self.render_memory, None);
            self.render_memory = vk::DeviceMemory::null();
        }
        if !self.readback_buffer.is_null() {
            self.device.destroy_buffer(self.readback_buffer, None);
            self.readback_buffer = vk::Buffer::null();
        }
        if !self.readback_memory.is_null() {
            self.device.free_memory(self.readback_memory, None);
            self.readback_memory = vk::DeviceMemory::null();
        }

        let format = vk::Format::R8G8B8A8_UNORM;
        let image = self.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D).format(format)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1).array_layers(1).samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
                .initial_layout(vk::ImageLayout::UNDEFINED), None,
        )?;
        let requirements = self.device.get_image_memory_requirements(image);
        let memory_type = self.memory_type_index(
            requirements, vk::MemoryPropertyFlags::DEVICE_LOCAL
        )?;
        let memory = self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size).memory_type_index(memory_type), None
        )?;
        self.device.bind_image_memory(image, memory, 0)?;

        let view = self.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image).view_type(vk::ImageViewType::TYPE_2D).format(format)
                .subresource_range(vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1).layer_count(1)), None
        )?;
        let framebuffer = self.device.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(self.render_pass).attachments(std::slice::from_ref(&view))
                .width(width).height(height).layers(1), None
        )?;

        let readback = self.device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(readback_size)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE), None,
        )?;
        let readback_requirements = self.device.get_buffer_memory_requirements(readback);
        let readback_type = self.memory_type_index(
            readback_requirements,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        let readback_memory = self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(readback_requirements.size)
                .memory_type_index(readback_type), None,
        )?;
        self.device.bind_buffer_memory(readback, readback_memory, 0)?;

        self.render_image = image;
        self.render_memory = memory;
        self.render_view = view;
        self.render_framebuffer = framebuffer;
        self.render_width = width;
        self.render_height = height;
        self.render_layout = vk::ImageLayout::UNDEFINED;
        self.readback_buffer = readback;
        self.readback_memory = readback_memory;
        self.readback_size = readback_size;
        Ok(())
    }

    unsafe fn render_command_frame(
        &mut self,
        commands: &[super::Command],
        width: u32,
        height: u32,
        present_image: Option<(vk::Image, u32)>,
        command_buffer: vk::CommandBuffer,
        submit_fence: vk::Fence,
        acquire_semaphore: vk::Semaphore,
    ) -> Result<(u32, Vec<u8>), VulkanError> {
        let result = (|| -> Result<(u32, Vec<u8>), VulkanError> {
            let (image, framebuffer, readback, readback_size, direct_present) = if let Some((present_image, present_index)) = present_image {
                let framebuffer = *self.swapchain_framebuffers.get(present_index as usize)
                    .ok_or(VulkanError::SwapchainUnavailable)?;
                (present_image, framebuffer, vk::Buffer::null(), 0, true)
            } else {
                self.ensure_render_targets(width, height)?;
                (self.render_image, self.render_framebuffer, self.readback_buffer, self.readback_size, false)
            };

            let mut state_layout = super::VertexLayout::empty();
            let mut state_index_type = super::IndexType::Uint16;
            let mut state_blend = super::BlendMode::Disabled;
            let mut state_depth = false;
            let mut draw_count = 0u32;
            let mut draw_keys = Vec::new();
            let mut seen_pipeline_keys = std::collections::HashSet::new();
            for command in commands {
                match command {
                    super::Command::SetVertexLayout(value) => state_layout = *value,
                    super::Command::SetIndexType(value) => state_index_type = *value,
                    super::Command::SetBlend(value) =>
                        state_blend = *value,
                    super::Command::SetDepthTest(value) => state_depth = *value,
                    super::Command::Draw { primitive, .. } |
                    super::Command::DrawIndexed { primitive, .. } => {
                        draw_count += 1;
                        let key = PipelineKey {
                            layout: state_layout,
                            primitive: *primitive,
                            blend: state_blend,
                            depth: state_depth,
                        };
                        if seen_pipeline_keys.insert(key) {
                            draw_keys.push(key);
                        }
                    }
                    _ => {}
                }
            }

            if draw_count == 0 {
                return Ok((0, Vec::new()));
            }

            let make_vertex_input = |layout: super::VertexLayout| {
                let binding = [vk::VertexInputBindingDescription::default()
                    .binding(0).stride(layout.stride.max(1))
                    .input_rate(vk::VertexInputRate::VERTEX)];
                let mut attributes = Vec::new();
                for attribute in layout.attributes.iter().flatten() {
                    let format = match attribute.ty {
                        super::VertexAttributeType::Float32 => vk::Format::R32_SFLOAT,
                        super::VertexAttributeType::Float32x2 => vk::Format::R32G32_SFLOAT,
                        super::VertexAttributeType::Float32x3 => vk::Format::R32G32B32_SFLOAT,
                        super::VertexAttributeType::Float32x4 => vk::Format::R32G32B32A32_SFLOAT,
                        super::VertexAttributeType::Uint8x4Norm => vk::Format::R8G8B8A8_UNORM,
                    };
                    attributes.push(vk::VertexInputAttributeDescription::default()
                        .location(attribute.location).binding(0).format(format)
                        .offset(attribute.offset));
                }
                (binding, attributes)
            };
            let viewport = [vk::Viewport {
                x: 0.0, y: 0.0, width: width.max(1) as f32, height: height.max(1) as f32,
                min_depth: 0.0, max_depth: 1.0,
            }];
            let scissor = [vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D { width: width.max(1), height: height.max(1) },
            }];
            let viewport_state = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1).scissor_count(1);
            let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic_state = vk::PipelineDynamicStateCreateInfo::default()
                .dynamic_states(&dynamic_states);
            let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL).cull_mode(vk::CullModeFlags::NONE)
                .front_face(vk::FrontFace::COUNTER_CLOCKWISE).line_width(1.0);
            let multisample = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);
            let _depth_state = vk::PipelineDepthStencilStateCreateInfo::default()
                .depth_test_enable(state_depth).depth_write_enable(state_depth)
                .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
            let shader_entry = CString::new("main").unwrap();
            let stages = [
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::VERTEX).module(self.vert_module)
                    .name(shader_entry.as_c_str()),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT).module(self.frag_module)
                    .name(shader_entry.as_c_str()),
            ];

            self.device.reset_command_buffer(
                command_buffer, vk::CommandBufferResetFlags::empty()
            )?;
            self.device.begin_command_buffer(
                command_buffer,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            if direct_present {
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::DependencyFlags::empty(),
                    &[], &[],
                    &[vk::ImageMemoryBarrier::default()
                        .image(image)
                        .src_access_mask(vk::AccessFlags::empty())
                        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                        .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                        .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .subresource_range(vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1))],
                );
            } else if self.render_layout != vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL {
                let (src_stage, src_access) = if self.render_layout == vk::ImageLayout::UNDEFINED {
                    (vk::PipelineStageFlags::TOP_OF_PIPE, vk::AccessFlags::empty())
                } else {
                    (vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::TRANSFER_READ)
                };
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    src_stage,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[vk::ImageMemoryBarrier::default()
                        .image(image)
                        .src_access_mask(src_access)
                        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                        .old_layout(self.render_layout)
                        .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .subresource_range(vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .level_count(1).layer_count(1))],
                );
                self.render_layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
            }
            let clear = [vk::ClearValue {
                color: vk::ClearColorValue { float32: [0.03, 0.05, 0.08, 1.0] },
            }];
            self.device.cmd_set_viewport(command_buffer, 0, &viewport);
            self.device.cmd_set_scissor(command_buffer, 0, &scissor);
            self.device.cmd_begin_render_pass(
                command_buffer,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass).framebuffer(framebuffer)
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: vk::Extent2D { width: width.max(1), height: height.max(1) },
                    }).clear_values(&clear),
                vk::SubpassContents::INLINE,
            );

            let topology = |primitive: super::Primitive| match primitive {
                super::Primitive::Points => vk::PrimitiveTopology::POINT_LIST,
                super::Primitive::Lines => vk::PrimitiveTopology::LINE_LIST,
                super::Primitive::Triangles => vk::PrimitiveTopology::TRIANGLE_LIST,
                super::Primitive::TriangleStrip => vk::PrimitiveTopology::TRIANGLE_STRIP,
                super::Primitive::TriangleFan => vk::PrimitiveTopology::TRIANGLE_FAN,
            };

            for key in draw_keys {
                if self.pipelines.contains_key(&key) { continue; }
                let (binding, attributes) = make_vertex_input(key.layout);
                let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
                    .vertex_binding_descriptions(&binding)
                    .vertex_attribute_descriptions(&attributes);
                let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                    .topology(topology(key.primitive));
                let key_blend_attachment = match key.blend {
                    super::BlendMode::Alpha =>
                    vk::PipelineColorBlendAttachmentState::default()
                        .blend_enable(true)
                        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                        .src_alpha_blend_factor(vk::BlendFactor::ONE)
                        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                        .color_write_mask(vk::ColorComponentFlags::R | vk::ColorComponentFlags::G |
                            vk::ColorComponentFlags::B | vk::ColorComponentFlags::A)
                ,
                    super::BlendMode::Additive => vk::PipelineColorBlendAttachmentState::default()
                        .blend_enable(true)
                        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                        .dst_color_blend_factor(vk::BlendFactor::ONE)
                        .src_alpha_blend_factor(vk::BlendFactor::ONE)
                        .dst_alpha_blend_factor(vk::BlendFactor::ONE)
                        .color_write_mask(vk::ColorComponentFlags::R | vk::ColorComponentFlags::G |
                            vk::ColorComponentFlags::B | vk::ColorComponentFlags::A),
                    super::BlendMode::Disabled => vk::PipelineColorBlendAttachmentState::default()
                        .color_write_mask(vk::ColorComponentFlags::R | vk::ColorComponentFlags::G |
                            vk::ColorComponentFlags::B | vk::ColorComponentFlags::A),
                };
                let key_blend_state = vk::PipelineColorBlendStateCreateInfo::default()
                    .attachments(std::slice::from_ref(&key_blend_attachment));
                let key_depth_state = vk::PipelineDepthStencilStateCreateInfo::default()
                    .depth_test_enable(key.depth).depth_write_enable(key.depth)
                    .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
                let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
                    .stages(&stages)
                    .vertex_input_state(&vertex_input)
                    .input_assembly_state(&input_assembly)
                    .viewport_state(&viewport_state)
                    .rasterization_state(&rasterization)
                    .multisample_state(&multisample)
                    .depth_stencil_state(&key_depth_state)
                    .color_blend_state(&key_blend_state)
                    .dynamic_state(&dynamic_state)
                    .layout(self.pipeline_layout)
                    .render_pass(self.render_pass).subpass(0);
                let pipeline = self.device.create_graphics_pipelines(
                    self.pipeline_cache, std::slice::from_ref(&pipeline_info), None
                ).map_err(|(_, error)| VulkanError::Vk(error))?[0];
                self.pipelines.insert(key, pipeline);
            }

            let mut current_vertex = None;
            let mut current_index = None;
            let mut bound_vertex = None;
            let mut current_index_binding = None;
            let mut current_pipeline_key = None;
            let mut current_layout = super::VertexLayout::empty();
            let mut current_blend = super::BlendMode::Disabled;
            let mut current_depth = false;
            let mut current_index_type = state_index_type;
            for command in commands {
                match command {
                    super::Command::BindVertexBuffer(id) => current_vertex = Some(*id),
                    super::Command::BindIndexBuffer(id) => current_index = Some(*id),
                    super::Command::SetVertexLayout(value) => current_layout = *value,
                    super::Command::SetBlend(value) => current_blend = *value,
                    super::Command::SetDepthTest(value) => current_depth = *value,
                    super::Command::SetIndexType(value) => current_index_type = *value,
                    super::Command::Draw { primitive, first, count } => {
                        let Some(id) = current_vertex else { continue };
                        let Some(buffer) = self.buffers.get(&id) else { continue };
                        let key = PipelineKey {
                            layout: current_layout,
                            primitive: *primitive,
                            blend: current_blend,
                            depth: current_depth,
                        };
                        let Some(&pipeline) = self.pipelines.get(&key) else { continue };
                        if current_pipeline_key != Some(key) {
                            self.device.cmd_bind_pipeline(
                                command_buffer, vk::PipelineBindPoint::GRAPHICS, pipeline
                            );
                            current_pipeline_key = Some(key);
                        }
                        if bound_vertex != Some(id) {
                            self.device.cmd_bind_vertex_buffers(
                                command_buffer, 0, &[buffer.buffer], &[0]
                            );
                            bound_vertex = Some(id);
                        }
                        self.device.cmd_draw(
                            command_buffer, *count, 1, *first, 0
                        );
                    }
                    super::Command::DrawIndexed { primitive, count, index_offset } => {
                        let Some(vertex_id) = current_vertex else { continue };
                        let Some(index_id) = current_index else { continue };
                        let Some(vertex) = self.buffers.get(&vertex_id) else { continue };
                        let Some(index) = self.buffers.get(&index_id) else { continue };
                        let key = PipelineKey {
                            layout: current_layout,
                            primitive: *primitive,
                            blend: current_blend,
                            depth: current_depth,
                        };
                        let Some(&pipeline) = self.pipelines.get(&key) else { continue };
                        if current_pipeline_key != Some(key) {
                            self.device.cmd_bind_pipeline(
                                command_buffer, vk::PipelineBindPoint::GRAPHICS, pipeline
                            );
                            current_pipeline_key = Some(key);
                        }
                        let vk_index = match current_index_type {
                            super::IndexType::Uint16 => vk::IndexType::UINT16,
                            super::IndexType::Uint32 => vk::IndexType::UINT32,
                            super::IndexType::Uint8 => continue,
                        };
                        if bound_vertex != Some(vertex_id) {
                            self.device.cmd_bind_vertex_buffers(
                                command_buffer, 0, &[vertex.buffer], &[0]
                            );
                            bound_vertex = Some(vertex_id);
                        }
                        let index_binding = (index_id, *index_offset as u64, vk_index);
                        if current_index_binding != Some(index_binding) {
                            self.device.cmd_bind_index_buffer(
                                command_buffer, index.buffer, *index_offset as u64, vk_index
                            );
                            current_index_binding = Some(index_binding);
                        }
                        self.device.cmd_draw_indexed(
                            command_buffer, *count, 1, 0, 0, 0
                        );
                    }
                    _ => {}
                }
            }

            self.device.cmd_end_render_pass(command_buffer);
            if !direct_present {
            self.device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[], &[],
                &[vk::ImageMemoryBarrier::default()
                    .image(image)
                    .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .subresource_range(vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1))],
            );

            }

            if direct_present {

                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::DependencyFlags::empty(),
                    &[], &[],
                    &[vk::ImageMemoryBarrier::default()
                        .image(image)
                        .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                        .dst_access_mask(vk::AccessFlags::empty())
                        .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                        .subresource_range(vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1))],
                );
                self.device.end_command_buffer(command_buffer)?;
                let command_buffers = [command_buffer];
                let wait = [acquire_semaphore];
                let signal = [self.present_semaphores[present_image.map(|(_, index)| index as usize).ok_or(VulkanError::SwapchainUnavailable)?]];
                let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
                self.device.queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().wait_semaphores(&wait).wait_dst_stage_mask(&stages)
                        .command_buffers(&command_buffers).signal_semaphores(&signal)],
                    submit_fence,
                )?;
                let loader = self.swapchain_loader.as_ref().ok_or(VulkanError::SwapchainUnavailable)?;
                let swapchains = [self.swapchain];
                let indices = [present_image.map(|(_, index)| index).ok_or(VulkanError::SwapchainUnavailable)?];
                loader.queue_present(self.queue, &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signal).swapchains(&swapchains).image_indices(&indices))?;
                self.render_layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
                Ok((draw_count, Vec::new()))
            } else {
                self.device.cmd_copy_image_to_buffer(
                    command_buffer, image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, readback,
                    &[vk::BufferImageCopy::default().buffer_offset(0).buffer_row_length(0).buffer_image_height(0)
                        .image_subresource(vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1))
                        .image_extent(vk::Extent3D { width: width.max(1), height: height.max(1), depth: 1 })],
                );
                self.device.end_command_buffer(command_buffer)?;
                let command_buffers = [command_buffer];
                self.device.queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().command_buffers(&command_buffers)],
                    submit_fence,
                )?;
                self.device.wait_for_fences(&[submit_fence], true, u64::MAX)?;
                let mapped = self.device.map_memory(self.readback_memory, 0, readback_size, vk::MemoryMapFlags::empty())?;
                let mut pixels = vec![0u8; readback_size as usize];
                std::ptr::copy_nonoverlapping(mapped.cast::<u8>(), pixels.as_mut_ptr(), pixels.len());
                self.device.unmap_memory(self.readback_memory);
                self.render_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
                Ok((draw_count, pixels))
            }
        })();

        result
    }

    pub unsafe fn render_frame_pixels(
        &mut self,
        commands: &[super::Command],
        width: u32,
        height: u32,
    ) -> Result<(u32, Vec<u8>), VulkanError> {
        let frame = self.current_frame;
        let fence = self.frame_fences[frame];
        self.device.wait_for_fences(&[fence], true, u64::MAX)?;

        for staging in self.deferred_upload_staging[frame].drain(..) {
            self.staging_pool.push(staging);
        }

        for command in commands {
            match command {
                super::Command::CreateBuffer { id, usage, size } =>
                    self.create_buffer(*id, *usage, *size)?,
                super::Command::UploadBuffer { id, offset, data } =>
                    self.upload_buffer(*id, *offset, data)?,
                super::Command::CreateTexture { id, width, height, format } =>
                    self.create_texture(*id, *width, *height, *format)?,
                super::Command::UploadTexture { id, x, y, width, height, data } =>
                    self.upload_texture(*id, *x, *y, *width, *height, data)?,
                _ => {}
            }
        }
        self.flush_upload_batch()?;
        let command_buffer = self.command_buffers[frame];
        self.render_command_frame(
            commands, width, height, None, command_buffer, fence, vk::Semaphore::null()
        )
    }

    pub unsafe fn submit_commands(
        &mut self, commands: &[super::Command],
    ) -> Result<(), VulkanError> {
        self.render_frame_pixels(commands, 1280, 720)?;
        self.current_frame = (self.current_frame + 1) % self.command_buffers.len();
        Ok(())
    }
}

impl super::Backend for VulkanBackend {
    type Error = VulkanError;
    fn submit(&mut self, commands: &[super::Command]) -> Result<(), Self::Error> {
        unsafe { self.submit_commands(commands) }
    }
    fn present(&mut self) -> Result<(), Self::Error> { Ok(()) }
}

impl Drop for VulkanBackend {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Ok(path) = std::env::var("GLIDE_PIPELINE_CACHE") {
                if let Ok(data) = self.device.get_pipeline_cache_data(self.pipeline_cache) {
                    if let Some(parent) = std::path::Path::new(&path).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(path, data);
                }
            }
            for (_, texture) in self.textures.drain() {
                self.device.destroy_image(texture.image, None);
                self.device.free_memory(texture.memory, None);
            }
            for (buffer, memory, _) in self.pending_upload_staging.drain(..) {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
            }
            for deferred in &mut self.deferred_upload_staging {
                for (buffer, memory, _) in deferred.drain(..) {
                    self.device.destroy_buffer(buffer, None);
                    self.device.free_memory(memory, None);
                }
            }
            for (buffer, memory, _) in self.staging_pool.drain(..) {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
            }
            for (_, buffer) in self.buffers.drain() {
                self.device.destroy_buffer(buffer.buffer, None);
                self.device.free_memory(buffer.memory, None);
            }
            if !self.render_framebuffer.is_null() {
                self.device.destroy_framebuffer(self.render_framebuffer, None);
            }
            if !self.render_view.is_null() {
                self.device.destroy_image_view(self.render_view, None);
            }
            if !self.render_image.is_null() {
                self.device.destroy_image(self.render_image, None);
            }
            if !self.render_memory.is_null() {
                self.device.free_memory(self.render_memory, None);
            }
            if !self.readback_buffer.is_null() {
                self.device.destroy_buffer(self.readback_buffer, None);
            }
            if !self.readback_memory.is_null() {
                self.device.free_memory(self.readback_memory, None);
            }
            for (_, pipeline) in self.pipelines.drain() {
                self.device.destroy_pipeline(pipeline, None);
            }
            self.device.destroy_pipeline_layout(self.pipeline_layout, None);
            self.device.destroy_shader_module(self.frag_module, None);
            self.device.destroy_shader_module(self.vert_module, None);
            self.device.destroy_render_pass(self.render_pass, None);
            for (buffer, memory, _) in self.pending_upload_staging.drain(..) {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
            }
            for staging in self.deferred_upload_staging.drain(..) {
                for (buffer, memory, _) in staging {
                    self.device.destroy_buffer(buffer, None);
                    self.device.free_memory(memory, None);
                }
            }
            for fence in self.frame_fences.drain(..) { self.device.destroy_fence(fence, None); }
            for semaphore in self.acquire_semaphores.drain(..) { self.device.destroy_semaphore(semaphore, None); }
            for semaphore in self.present_semaphores.drain(..) { self.device.destroy_semaphore(semaphore, None); }
            self.device.destroy_pipeline_cache(self.pipeline_cache, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}