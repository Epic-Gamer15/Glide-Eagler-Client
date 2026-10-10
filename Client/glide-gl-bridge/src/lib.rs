use glide_gfx::{
    BlendMode, BufferUsage, Command, CommandBuffer, IndexType, Primitive,
    ResourceId, VertexAttribute, VertexAttributeType, VertexLayout,
};
use std::{
    ffi::{c_char, c_void, CString},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::Instant,
};

#[cfg(target_os = "linux")]
use x11::xlib;

// Forward OpenGL calls to the driver while recording a parallel Glide command stream.
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const GL_STATIC_DRAW: u32 = 0x88E4;
const GL_DYNAMIC_DRAW: u32 = 0x88E8;
const GL_STREAM_DRAW: u32 = 0x88E0;
const GL_BLEND: u32 = 0x0BE2;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_TRIANGLES: u32 = 0x0004;
const GL_TRIANGLE_STRIP: u32 = 0x0005;
const GL_TRIANGLE_FAN: u32 = 0x0006;
const GL_LINES: u32 = 0x0001;
const GL_POINTS: u32 = 0x0000;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_RGBA: u32 = 0x1908;
const GL_BGRA: u32 = 0x80E1;
const GL_RGB: u32 = 0x1907;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_UNSIGNED_SHORT: u32 = 0x1403;
const GL_UNSIGNED_INT: u32 = 0x1405;
const GL_VERTEX_ARRAY: u32 = 0x8074;
const GL_COLOR_ARRAY: u32 = 0x8076;
const GL_TEXTURE_COORD_ARRAY: u32 = 0x8078;
const GL_FLOAT: u32 = 0x1406;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;

type GlBindBuffer = unsafe extern "system" fn(u32, u32);
type GlBufferData = unsafe extern "system" fn(u32, isize, *const c_void, u32);
type GlDrawArrays = unsafe extern "system" fn(u32, i32, i32);
type GlDrawElements = unsafe extern "system" fn(u32, i32, u32, *const c_void);
type GlEnable = unsafe extern "system" fn(u32);
type GlDisable = unsafe extern "system" fn(u32);
type GlBlendFunc = unsafe extern "system" fn(u32, u32);
type GlGenTextures = unsafe extern "system" fn(i32, *mut u32);
type GlDeleteTextures = unsafe extern "system" fn(i32, *const u32);
type GlBindTexture = unsafe extern "system" fn(u32, u32);
type GlTexImage2D = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type GlTexSubImage2D = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type GlTexParameteri = unsafe extern "system" fn(u32, u32, i32);
type GlEnableClientState = unsafe extern "system" fn(u32);
type GlDisableClientState = unsafe extern "system" fn(u32);
type GlVertexPointer = unsafe extern "system" fn(i32, u32, i32, *const c_void);
type GlColorPointer = unsafe extern "system" fn(i32, u32, i32, *const c_void);
type GlTexCoordPointer = unsafe extern "system" fn(i32, u32, i32, *const c_void);
type GlCreateShader = unsafe extern "system" fn(u32) -> u32;
type GlCreateProgram = unsafe extern "system" fn() -> u32;
type GlUseProgram = unsafe extern "system" fn(u32);
type GlFinish = unsafe extern "system" fn();
type GlXSwapBuffers = unsafe extern "C" fn(*mut c_void, u64);

static mut REAL_BIND_BUFFER: Option<GlBindBuffer> = None;
static mut REAL_BUFFER_DATA: Option<GlBufferData> = None;
static mut REAL_DRAW_ARRAYS: Option<GlDrawArrays> = None;
static mut REAL_DRAW_ELEMENTS: Option<GlDrawElements> = None;
static mut REAL_ENABLE: Option<GlEnable> = None;
static mut REAL_DISABLE: Option<GlDisable> = None;
static mut REAL_BLEND_FUNC: Option<GlBlendFunc> = None;
static mut REAL_GEN_TEXTURES: Option<GlGenTextures> = None;
static mut REAL_DELETE_TEXTURES: Option<GlDeleteTextures> = None;
static mut REAL_BIND_TEXTURE: Option<GlBindTexture> = None;
static mut REAL_TEX_IMAGE_2D: Option<GlTexImage2D> = None;
static mut REAL_TEX_SUB_IMAGE_2D: Option<GlTexSubImage2D> = None;
static mut REAL_TEX_PARAMETERI: Option<GlTexParameteri> = None;
static mut REAL_ENABLE_CLIENT_STATE: Option<GlEnableClientState> = None;
static mut REAL_DISABLE_CLIENT_STATE: Option<GlDisableClientState> = None;
static mut REAL_VERTEX_POINTER: Option<GlVertexPointer> = None;
static mut REAL_COLOR_POINTER: Option<GlColorPointer> = None;
static mut REAL_TEX_COORD_POINTER: Option<GlTexCoordPointer> = None;
static mut REAL_CREATE_SHADER: Option<GlCreateShader> = None;
static mut REAL_CREATE_PROGRAM: Option<GlCreateProgram> = None;
static mut REAL_USE_PROGRAM: Option<GlUseProgram> = None;
static mut REAL_FINISH: Option<GlFinish> = None;
static mut REAL_GLX_SWAP_BUFFERS: Option<GlXSwapBuffers> = None;
#[cfg(feature = "vulkan")]
static mut VULKAN_BACKEND: Option<glide_gfx::vulkan::VulkanBackend> = None;
#[cfg(feature = "vulkan")]
static mut VULKAN_INIT_ATTEMPTED: bool = false;

static RENDER_SCHEDULER: OnceLock<Mutex<glide_gfx::scheduler::RenderScheduler>> = OnceLock::new();
static SCHEDULER_LAST_UPDATE: OnceLock<Mutex<Instant>> = OnceLock::new();

fn refresh_scheduler_budget() {
    let now = Instant::now();
    let gate = SCHEDULER_LAST_UPDATE.get_or_init(|| Mutex::new(now));
    let mut last = gate.lock().expect("scheduler update mutex poisoned");
    if now.duration_since(*last).as_millis() < 4 {
        return;
    }
    *last = now;
    if let Ok(mut sched) = scheduler().lock() {
        let local_cpu = process_cpu_busy();
        let runtime_cpu = runtime_probe_value("cpu_busy").unwrap_or(0.0);
        let cpu_busy = local_cpu.max(runtime_cpu);
        let gpu_proxy = unsafe {
            (telemetry().last_frame_ms / 16.6667).clamp(0.0, 1.0) as f32
        };
        sched.update_budget(cpu_busy, gpu_proxy);
    }
}

fn runtime_probe_value(key: &str) -> Option<f32> {
    let path = std::env::var("GLIDE_RUNTIME_PROBE")
        .unwrap_or_else(|_| "/tmp/glide-runtime.json".to_string());
    let data = std::fs::read_to_string(path).ok()?;
    let marker = format!("\"{}\":", key);
    let start = data.find(&marker)? + marker.len();
    let tail = &data[start..];
    let end = tail.find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != '-')
        .unwrap_or(tail.len());
    tail[..end].parse::<f32>().ok()
}

static CPU_SAMPLE: OnceLock<Mutex<(Instant, u64)>> = OnceLock::new();

fn scheduler() -> &'static Mutex<glide_gfx::scheduler::RenderScheduler> {
    RENDER_SCHEDULER.get_or_init(|| Mutex::new(glide_gfx::scheduler::RenderScheduler::new(0)))
}

fn process_cpu_nanos() -> u64 {
    unsafe {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        if libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) == 0 {
            (ts.tv_sec as u64)
                .saturating_mul(1_000_000_000)
                .saturating_add(ts.tv_nsec as u64)
        } else {
            0
        }
    }
}

fn process_cpu_busy() -> f32 {
    let now = Instant::now();
    let cpu = process_cpu_nanos();
    let sample = CPU_SAMPLE.get_or_init(|| Mutex::new((now, cpu)));
    let mut sample = sample.lock().expect("CPU sample mutex poisoned");
    let wall_ns = now.duration_since(sample.0).as_nanos() as u64;
    let cpu_ns = cpu.saturating_sub(sample.1);
    sample.0 = now;
    sample.1 = cpu;
    if wall_ns == 0 {
        0.0
    } else {
        (cpu_ns as f64 / wall_ns as f64).clamp(0.0, 1.0) as f32
    }
}

#[inline]
fn vulkan_mode() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("GLIDE_VULKAN_BRIDGE").ok().as_deref() == Some("1"))
}

#[derive(Clone, Copy, Debug, Default)]
struct BridgeTelemetry {
    frames: u64,
    last_frame_ms: f64,
    max_frame_ms: f64,
    commands_before_optimize: u64,
    commands_after_optimize: u64,
    last_draws: u64,
    last_uploads: u64,
    last_upload_bytes: u64,
    scheduler_workers: u64,
    scheduler_queued: u64,
    scheduler_cpu_busy: f32,
    scheduler_gpu_busy: f32,
}

impl BridgeTelemetry {
    const fn new() -> Self {
        Self {
            frames: 0,
            last_frame_ms: 0.0,
            max_frame_ms: 0.0,
            commands_before_optimize: 0,
            commands_after_optimize: 0,
            last_draws: 0,
            last_uploads: 0,
            last_upload_bytes: 0,
            scheduler_workers: 0,
            scheduler_queued: 0,
            scheduler_cpu_busy: 0.0,
            scheduler_gpu_busy: 0.0,
        }
    }
}

struct AsyncFrame {
    display: usize,
    drawable: u64,
    commands: CommandBuffer,
    commands_before: u64,
    draws_before: u64,
    uploads_before: u64,
}

struct AsyncRenderer {
    pending: Mutex<Option<AsyncFrame>>,
    wake: Condvar,
}

static ASYNC_RENDERER: OnceLock<Arc<AsyncRenderer>> = OnceLock::new();

fn async_renderer() -> &'static Arc<AsyncRenderer> {
    ASYNC_RENDERER.get_or_init(|| {
        let renderer = Arc::new(AsyncRenderer {
            pending: Mutex::new(None),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&renderer);
        std::thread::Builder::new()
            .name("glide-vulkan-render".to_string())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                unsafe {
                    libc::nice(2);
                }

                loop {
                    let frame = {
                        let mut pending = worker.pending.lock().expect("async renderer mutex poisoned");
                        while pending.is_none() {
                            pending = worker.wake.wait(pending)
                                .expect("async renderer condvar poisoned");
                        }
                        pending.take().expect("async renderer frame disappeared")
                    };
                    unsafe {
                        render_async_frame(frame);
                    }
                }
            })
            .expect("failed to start Glide Vulkan render thread");
        renderer
    })
}

fn coalesce_resource_commands(old: &CommandBuffer, new: &mut CommandBuffer) {
    let mut resources = Vec::with_capacity(old.commands().len());
    for command in old.commands() {
        match command {
            Command::CreateBuffer { .. }
            | Command::UploadBuffer { .. }
            | Command::CreateTexture { .. }
            | Command::UploadTexture { .. } => resources.push(command.clone()),
            _ => {}
        }
    }
    if resources.is_empty() {
        return;
    }

    let mut merged = CommandBuffer::new();
    merged.reserve(resources.len() + new.commands().len());
    merged.extend(resources);
    merged.extend(new.commands().iter().cloned());
    *new = merged;
}

unsafe fn enqueue_async_frame(
    display: *mut c_void,
    drawable: u64,
    mut commands: CommandBuffer,
    commands_before: u64,
    draws_before: u64,
    uploads_before: u64,
) {
    let renderer = async_renderer();
    let mut pending = renderer.pending.lock().expect("async renderer mutex poisoned");

    let frame = AsyncFrame {
        display: display as usize,
        drawable,
        commands: std::mem::replace(&mut commands, CommandBuffer::new()),
        commands_before,
        draws_before,
        uploads_before,
    };

    if let Some(old) = pending.replace(frame) {
        if let Some(newer) = pending.as_mut() {
            coalesce_resource_commands(&old.commands, &mut newer.commands);
        }
    }
    renderer.wake.notify_one();
}

fn render_dimensions() -> (u32, u32) {
    static DIMENSIONS: OnceLock<(u32, u32)> = OnceLock::new();
    *DIMENSIONS.get_or_init(|| {
        let width = std::env::var("GLIDE_RENDER_WIDTH")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|&value| value > 0)
            .unwrap_or(854);
        let height = std::env::var("GLIDE_RENDER_HEIGHT")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|&value| value > 0)
            .unwrap_or(480);
        (width, height)
    })
}

// Render a captured frame on the background worker thread. This keeps the GL call path fast while a
// Vulkan-backed worker consumes the recorded command stream asynchronously.
unsafe fn render_async_frame(frame: AsyncFrame) {
    let display = frame.display as *mut c_void;
    let frame_start = Instant::now();
    let (width, height) = render_dimensions();

    #[cfg(feature = "vulkan")]
    if vulkan_mode() {
        if !VULKAN_INIT_ATTEMPTED {
            VULKAN_INIT_ATTEMPTED = true;
            match glide_gfx::vulkan::VulkanBackend::new() {
                Ok(backend) => {
                    eprintln!(
                        "GLIDE_VULKAN_BRIDGE=ready device={} type={:?}",
                        backend.device_name(),
                        backend.device_type()
                    );
                    VULKAN_BACKEND = Some(backend);
                }
                Err(error) => {
                    eprintln!("GLIDE_VULKAN_BRIDGE=unavailable error={error:?}");
                    if let Some(real) = REAL_GLX_SWAP_BUFFERS {
                        real(display, frame.drawable);
                    }
                    return;
                }
            }
        }

        if let Some(backend) = VULKAN_BACKEND.as_mut() {
            let mut commands = frame.commands;
            commands.optimize();
            let mut frame_draws = 0u64;
            let mut frame_uploads = 0u64;
            let mut upload_bytes = 0u64;
            for command in commands.commands() {
                match command {
                    Command::Draw { .. } | Command::DrawIndexed { .. } => frame_draws += 1,
                    Command::UploadBuffer { data, .. } | Command::UploadTexture { data, .. } => {
                        frame_uploads += 1;
                        upload_bytes = upload_bytes.saturating_add(data.len() as u64);
                    }
                    _ => {}
                }
            }
            {
                let mut stats = telemetry();
                stats.commands_before_optimize = frame.commands_before;
                stats.commands_after_optimize = commands.len() as u64;
                stats.last_draws = frame_draws;
                stats.last_uploads = frame_uploads;
                stats.last_upload_bytes = upload_bytes;
            }

            if !commands.is_empty() {
                if backend.attach_x11(display.cast(), frame.drawable, width, height).is_ok() {
                    match backend.render_presented_frame(commands.commands(), width, height) {
                        Ok(_) => {
                            record_frame_telemetry(frame_start);
                            return;
                        }
                        Err(error) => eprintln!(
                            "GLIDE_VULKAN_FRAME=present_failed error={error:?}"
                        ),
                    }
                }
            }
        }
    }

    record_frame_telemetry(frame_start);
}

static BRIDGE_TELEMETRY: OnceLock<Mutex<BridgeTelemetry>> = OnceLock::new();

fn telemetry() -> std::sync::MutexGuard<'static, BridgeTelemetry> {
    BRIDGE_TELEMETRY
        .get_or_init(|| Mutex::new(BridgeTelemetry::new()))
        .lock()
        .expect("bridge telemetry mutex poisoned")
}

/// Tracks the live OpenGL state that the bridge is translating into Glide commands.
#[derive(Default)]
struct BridgeState {
    array_buffer: u32,
    element_buffer: u32,
    commands: CommandBuffer,
    draws: u64,
    uploads: u64,
    texture: u32,
    shader_program: u32,
    blend_mode: BlendMode,
    depth_enabled: bool,
    vertex_layout: VertexLayout,
    vertex_enabled: bool,
    color_enabled: bool,
    texcoord_enabled: bool,
    vertex_stride: i32,
    vertex_offset: u32,
    color_stride: i32,
    color_offset: u32,
    texcoord_stride: i32,
    texcoord_offset: u32,
    telemetry: BridgeTelemetry,
}

static mut STATE: BridgeState = BridgeState {
    array_buffer: 0,
    element_buffer: 0,
    commands: CommandBuffer::new(),
    draws: 0,
    uploads: 0,
    texture: 0,
    shader_program: 0,
    blend_mode: BlendMode::Disabled,
    depth_enabled: false,
    vertex_layout: VertexLayout::empty(),
    vertex_enabled: false,
    color_enabled: false,
    texcoord_enabled: false,
    vertex_stride: 0,
    vertex_offset: 0,
    color_stride: 0,
    color_offset: 0,
    texcoord_stride: 0,
    texcoord_offset: 0,
    telemetry: BridgeTelemetry::new(),
};

unsafe fn real_symbol<T: Copy>(name: &str) -> T {
    #[link(name = "dl")]
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
    }
    const RTLD_NEXT: *mut c_void = -1isize as *mut c_void;
    let cname = CString::new(name).unwrap();
    let ptr = dlsym(RTLD_NEXT, cname.as_ptr());
    if ptr.is_null() {
        let lib = dlopen(b"libGL.so.1\0".as_ptr().cast(), 0x0001);
        let ptr = dlsym(lib, cname.as_ptr());
        assert!(!ptr.is_null(), "Glide GL bridge could not resolve {name}");
        return std::mem::transmute_copy(&ptr);
    }
    std::mem::transmute_copy(&ptr)
}

unsafe fn ensure_symbols() {
    static SYMBOLS_READY: OnceLock<()> = OnceLock::new();
    let _ = SYMBOLS_READY.get_or_init(|| {
        let _ = xlib::XInitThreads();
        unsafe {
            REAL_BIND_BUFFER = Some(real_symbol("glBindBuffer"));
            REAL_BUFFER_DATA = Some(real_symbol("glBufferData"));
            REAL_DRAW_ARRAYS = Some(real_symbol("glDrawArrays"));
            REAL_DRAW_ELEMENTS = Some(real_symbol("glDrawElements"));
            REAL_ENABLE = Some(real_symbol("glEnable"));
            REAL_DISABLE = Some(real_symbol("glDisable"));
            REAL_BLEND_FUNC = Some(real_symbol("glBlendFunc"));
            REAL_GEN_TEXTURES = Some(real_symbol("glGenTextures"));
            REAL_DELETE_TEXTURES = Some(real_symbol("glDeleteTextures"));
            REAL_BIND_TEXTURE = Some(real_symbol("glBindTexture"));
            REAL_TEX_IMAGE_2D = Some(real_symbol("glTexImage2D"));
            REAL_TEX_SUB_IMAGE_2D = Some(real_symbol("glTexSubImage2D"));
            REAL_TEX_PARAMETERI = Some(real_symbol("glTexParameteri"));
            REAL_ENABLE_CLIENT_STATE = Some(real_symbol("glEnableClientState"));
            REAL_DISABLE_CLIENT_STATE = Some(real_symbol("glDisableClientState"));
            REAL_VERTEX_POINTER = Some(real_symbol("glVertexPointer"));
            REAL_COLOR_POINTER = Some(real_symbol("glColorPointer"));
            REAL_TEX_COORD_POINTER = Some(real_symbol("glTexCoordPointer"));
            REAL_CREATE_SHADER = Some(real_symbol("glCreateShader"));
            REAL_CREATE_PROGRAM = Some(real_symbol("glCreateProgram"));
            REAL_USE_PROGRAM = Some(real_symbol("glUseProgram"));
            REAL_FINISH = Some(real_symbol("glFinish"));
            REAL_GLX_SWAP_BUFFERS = Some(real_symbol("glXSwapBuffers"));
        }
    });
}

fn primitive(mode: u32) -> Option<Primitive> {
    Some(match mode {
        GL_POINTS => Primitive::Points,
        GL_LINES => Primitive::Lines,
        GL_TRIANGLES => Primitive::Triangles,
        GL_TRIANGLE_STRIP => Primitive::TriangleStrip,
        GL_TRIANGLE_FAN => Primitive::TriangleFan,
        _ => return None,
    })
}

fn index_type(ty: u32) -> Option<IndexType> {
    match ty {
        GL_UNSIGNED_BYTE => Some(IndexType::Uint8),
        GL_UNSIGNED_SHORT => Some(IndexType::Uint16),
        GL_UNSIGNED_INT => Some(IndexType::Uint32),
        _ => None,
    }
}

fn usage(hint: u32) -> BufferUsage {
    match hint {
        GL_DYNAMIC_DRAW | GL_STREAM_DRAW => BufferUsage::Storage,
        GL_STATIC_DRAW => BufferUsage::Vertex,
        _ => BufferUsage::Vertex,
    }
}

#[no_mangle]
pub unsafe extern "system" fn glBindBuffer(target: u32, buffer: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_BIND_BUFFER { real(target, buffer); } }
    match target {
        GL_ARRAY_BUFFER => {
            if STATE.array_buffer != buffer {
                STATE.array_buffer = buffer;
                if buffer != 0 {
                    STATE.commands.push(Command::BindVertexBuffer(ResourceId::from_raw(buffer)));
                }
            }
        }
        GL_ELEMENT_ARRAY_BUFFER => {
            if STATE.element_buffer != buffer {
                STATE.element_buffer = buffer;
                if buffer != 0 {
                    STATE.commands.push(Command::BindIndexBuffer(ResourceId::from_raw(buffer)));
                }
            }
        }
        _ => {}
    }
}

#[no_mangle]
pub unsafe extern "system" fn glBufferData(target: u32, size: isize, data: *const c_void, hint: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_BUFFER_DATA { real(target, size, data, hint); } }
    if size <= 0 { return; }
    let buffer = match target {
        GL_ARRAY_BUFFER => STATE.array_buffer,
        GL_ELEMENT_ARRAY_BUFFER => STATE.element_buffer,
        _ => 0,
    };
    if buffer == 0 { return; }
    let id = ResourceId::from_raw(buffer);
    STATE.commands.push(Command::CreateBuffer { id, usage: usage(hint), size: size as usize });
    if !data.is_null() {
        let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size as usize).to_vec();
        STATE.commands.push(Command::UploadBuffer { id, offset: 0, data: bytes });
        STATE.uploads += 1;
    }
}

#[no_mangle]
pub unsafe extern "system" fn glEnable(cap: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_ENABLE { real(cap); } }
    if cap == GL_BLEND {
        if STATE.blend_mode != BlendMode::Alpha {
            STATE.blend_mode = BlendMode::Alpha;
            STATE.commands.push(Command::SetBlend(BlendMode::Alpha));
        }
    } else if cap == GL_DEPTH_TEST {
        if !STATE.depth_enabled {
            STATE.depth_enabled = true;
            STATE.commands.push(Command::SetDepthTest(true));
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn glDisable(cap: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_DISABLE { real(cap); } }
    if cap == GL_BLEND {
        if STATE.blend_mode != BlendMode::Disabled {
            STATE.blend_mode = BlendMode::Disabled;
            STATE.commands.push(Command::SetBlend(BlendMode::Disabled));
        }
    } else if cap == GL_DEPTH_TEST {
        if STATE.depth_enabled {
            STATE.depth_enabled = false;
            STATE.commands.push(Command::SetDepthTest(false));
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn glBlendFunc(src: u32, dst: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_BLEND_FUNC { real(src, dst); } }
    if STATE.blend_mode != BlendMode::Alpha {
        STATE.blend_mode = BlendMode::Alpha;
        STATE.commands.push(Command::SetBlend(BlendMode::Alpha));
    }
}

#[no_mangle]
pub unsafe extern "system" fn glDrawArrays(mode: u32, first: i32, count: i32) {
    ensure_symbols();
    if !vulkan_mode() {
        if let Some(real) = REAL_DRAW_ARRAYS { real(mode, first, count); }
    }
    if let Some(p) = primitive(mode) {
        STATE.commands.push(Command::Draw { primitive: p, first: first.max(0) as u32, count: count.max(0) as u32 });
        STATE.draws += 1;
    }
}

#[no_mangle]
pub unsafe extern "system" fn glDrawElements(mode: u32, count: i32, _index_type: u32, indices: *const c_void) {
    ensure_symbols();
    if !vulkan_mode() {
        if let Some(real) = REAL_DRAW_ELEMENTS { real(mode, count, _index_type, indices); }
    }
    if let Some(p) = primitive(mode) {
        if let Some(ty) = index_type(_index_type) {
            STATE.commands.push(Command::SetIndexType(ty));
        }
        let offset = if STATE.element_buffer != 0 { indices as usize as u32 } else { 0 };
        STATE.commands.push(Command::DrawIndexed { primitive: p, count: count.max(0) as u32, index_offset: offset });
        STATE.draws += 1;
    }
}

/// Returns the number of queued Glide commands currently waiting to be submitted.
#[no_mangle]
pub unsafe extern "system" fn glide_gl_bridge_flush() -> usize {
    STATE.commands.len()
}

#[no_mangle]
pub unsafe extern "system" fn glide_gl_bridge_draw_count() -> u64 { STATE.draws }

#[no_mangle]
pub unsafe extern "system" fn glide_gl_bridge_upload_count() -> u64 { STATE.uploads }

fn texture_format(format: u32, ty: u32) -> Option<(glide_gfx::TextureFormat, usize)> {
    if ty != GL_UNSIGNED_BYTE { return None; }
    match format {
        GL_RGBA => Some((glide_gfx::TextureFormat::Rgba8, 4)),
        GL_BGRA => Some((glide_gfx::TextureFormat::Bgra8, 4)),
        GL_RGB => Some((glide_gfx::TextureFormat::Rgb8, 3)),
        _ => None,
    }
}

#[no_mangle]
pub unsafe extern "system" fn glGenTextures(count: i32, textures: *mut u32) {
    ensure_symbols();
    if let Some(real) = REAL_GEN_TEXTURES { real(count, textures); }
}

#[no_mangle]
pub unsafe extern "system" fn glDeleteTextures(count: i32, textures: *const u32) {
    ensure_symbols();
    if let Some(real) = REAL_DELETE_TEXTURES { real(count, textures); }
}

#[no_mangle]
pub unsafe extern "system" fn glBindTexture(target: u32, texture: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_BIND_TEXTURE { real(target, texture); } }
    if target == GL_TEXTURE_2D && STATE.texture != texture {
        STATE.texture = texture;
        if texture != 0 {
            STATE.commands.push(Command::BindTexture {
                slot: 0,
                texture: ResourceId::from_raw(texture),
            });
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn glTexImage2D(
    target: u32, level: i32, internal_format: i32, width: i32, height: i32,
    border: i32, format: u32, ty: u32, pixels: *const c_void,
) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_TEX_IMAGE_2D {
        real(target, level, internal_format, width, height, border, format, ty, pixels);
        }
    }
    if target != GL_TEXTURE_2D || level != 0 || width <= 0 || height <= 0 || STATE.texture == 0 {
        return;
    }
    if let Some((gl_format, bpp)) = texture_format(format, ty) {
        STATE.commands.push(Command::CreateTexture {
            id: ResourceId::from_raw(STATE.texture),
            width: width as u32,
            height: height as u32,
            format: gl_format,
        });
        if !pixels.is_null() {
            let size = width as usize * height as usize * bpp;
            let data = std::slice::from_raw_parts(pixels.cast::<u8>(), size).to_vec();
            STATE.commands.push(Command::UploadTexture {
                id: ResourceId::from_raw(STATE.texture),
                x: 0, y: 0, width: width as u32, height: height as u32, data,
            });
            STATE.uploads += 1;
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn glTexSubImage2D(
    target: u32, level: i32, x: i32, y: i32, width: i32, height: i32,
    format: u32, ty: u32, pixels: *const c_void,
) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_TEX_SUB_IMAGE_2D {
        real(target, level, x, y, width, height, format, ty, pixels);
    } }
    if target != GL_TEXTURE_2D || level != 0 || STATE.texture == 0 ||
        x < 0 || y < 0 || width <= 0 || height <= 0 || pixels.is_null() {
        return;
    }
    if let Some((_, bpp)) = texture_format(format, ty) {
        let size = width as usize * height as usize * bpp;
        let data = std::slice::from_raw_parts(pixels.cast::<u8>(), size).to_vec();
        STATE.commands.push(Command::UploadTexture {
            id: ResourceId::from_raw(STATE.texture),
            x: x as u32, y: y as u32, width: width as u32, height: height as u32, data,
        });
        STATE.uploads += 1;
    }
}

#[no_mangle]
pub unsafe extern "system" fn glTexParameteri(target: u32, pname: u32, param: i32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_TEX_PARAMETERI { real(target, pname, param); } }
}

fn rebuild_legacy_layout() {
    unsafe {
        let mut layout = VertexLayout::empty();
        let stride = [STATE.vertex_stride, STATE.color_stride, STATE.texcoord_stride]
            .into_iter().filter(|value| *value > 0)
            .map(|value| value as u32).next().unwrap_or(0);
        if STATE.vertex_enabled {
            layout = layout.with_attribute(0, VertexAttribute {
                location: 0, offset: STATE.vertex_offset, ty: VertexAttributeType::Float32x3,
            });
        }
        if STATE.color_enabled {
            layout = layout.with_attribute(1, VertexAttribute {
                location: 1, offset: STATE.color_offset, ty: VertexAttributeType::Float32x4,
            });
        }
        if STATE.texcoord_enabled {
            layout = layout.with_attribute(2, VertexAttribute {
                location: 2, offset: STATE.texcoord_offset, ty: VertexAttributeType::Float32x2,
            });
        }
        layout.stride = stride;
        if STATE.vertex_layout != layout {
            STATE.vertex_layout = layout;
            STATE.commands.push(Command::SetVertexLayout(layout));
        }
    }
}
#[no_mangle]
pub unsafe extern "system" fn glEnableClientState(array: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_ENABLE_CLIENT_STATE { real(array); } }
    match array {
        GL_VERTEX_ARRAY => STATE.vertex_enabled = true,
        GL_COLOR_ARRAY => STATE.color_enabled = true,
        GL_TEXTURE_COORD_ARRAY => STATE.texcoord_enabled = true,
        _ => return,
    }
    rebuild_legacy_layout();
}

#[no_mangle]
pub unsafe extern "system" fn glDisableClientState(array: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_DISABLE_CLIENT_STATE { real(array); } }
    match array {
        GL_VERTEX_ARRAY => STATE.vertex_enabled = false,
        GL_COLOR_ARRAY => STATE.color_enabled = false,
        GL_TEXTURE_COORD_ARRAY => STATE.texcoord_enabled = false,
        _ => return,
    }
    rebuild_legacy_layout();
}

#[no_mangle]
pub unsafe extern "system" fn glVertexPointer(size: i32, ty: u32, stride: i32, pointer: *const c_void) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_VERTEX_POINTER { real(size, ty, stride, pointer); } }
    if ty == GL_FLOAT && size >= 2 {
        STATE.vertex_stride = stride;
        STATE.vertex_offset = pointer as usize as u32;
        rebuild_legacy_layout();
    }
}

#[no_mangle]
pub unsafe extern "system" fn glColorPointer(size: i32, ty: u32, stride: i32, pointer: *const c_void) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_COLOR_POINTER { real(size, ty, stride, pointer); } }
    if ty == GL_FLOAT && size >= 3 {
        STATE.color_stride = stride;
        STATE.color_offset = pointer as usize as u32;
        rebuild_legacy_layout();
    }
}

#[no_mangle]
pub unsafe extern "system" fn glTexCoordPointer(size: i32, ty: u32, stride: i32, pointer: *const c_void) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_TEX_COORD_POINTER { real(size, ty, stride, pointer); } }
    if ty == GL_FLOAT && size >= 2 {
        STATE.texcoord_stride = stride;
        STATE.texcoord_offset = pointer as usize as u32;
        rebuild_legacy_layout();
    }
}

#[no_mangle]
pub unsafe extern "system" fn glCreateShader(shader_type: u32) -> u32 {
    ensure_symbols();
    REAL_CREATE_SHADER.map(|f| f(shader_type)).unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "system" fn glCreateProgram() -> u32 {
    ensure_symbols();
    REAL_CREATE_PROGRAM.map(|f| f()).unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "system" fn glUseProgram(program: u32) {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_USE_PROGRAM { real(program); } }
    if STATE.shader_program != program {
        STATE.shader_program = program;
        if program != 0 {
            STATE.commands.push(Command::SetShaderProgram(ResourceId::from_raw(program)));
        }
    }
}

unsafe fn flush_vulkan_frame(display: *mut c_void, drawable: u64) -> bool {
    if !vulkan_mode() {
        return false;
    }

    let commands_before = STATE.commands.len() as u64;
    let draws_before = STATE.draws;
    let uploads_before = STATE.uploads;
    let commands = std::mem::replace(&mut STATE.commands, CommandBuffer::new());

    enqueue_async_frame(
        display,
        drawable,
        commands,
        commands_before,
        draws_before,
        uploads_before,
    );
    true
}

fn record_frame_telemetry(start: std::time::Instant) {
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    if let Ok(mut stats) = BRIDGE_TELEMETRY
        .get_or_init(|| Mutex::new(BridgeTelemetry::new()))
        .lock()
    {
        stats.frames = stats.frames.saturating_add(1);
        stats.last_frame_ms = ms;
        stats.max_frame_ms = stats.max_frame_ms.max(ms);
    }
    if let Ok(mut sched) = scheduler().lock() {
        sched.end_frame();
        let snapshot = sched.snapshot();
        if let Ok(mut stats) = BRIDGE_TELEMETRY
            .get_or_init(|| Mutex::new(BridgeTelemetry::new()))
            .lock()
        {
            stats.scheduler_workers = snapshot.workers as u64;
            stats.scheduler_queued = snapshot.queued as u64;
            stats.scheduler_cpu_busy = snapshot.cpu_busy;
            stats.scheduler_gpu_busy = snapshot.gpu_busy;
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn glide_gl_bridge_metrics(
    frames: *mut u64,
    last_frame_us: *mut u64,
    max_frame_us: *mut u64,
    commands_before: *mut u64,
    commands_after: *mut u64,
    draws: *mut u64,
    uploads: *mut u64,
) {
    if !frames.is_null() { *frames = telemetry().frames; }
    if !last_frame_us.is_null() { *last_frame_us = (telemetry().last_frame_ms * 1000.0) as u64; }
    if !max_frame_us.is_null() { *max_frame_us = (telemetry().max_frame_ms * 1000.0) as u64; }
    if !commands_before.is_null() { *commands_before = telemetry().commands_before_optimize; }
    if !commands_after.is_null() { *commands_after = telemetry().commands_after_optimize; }
    if !draws.is_null() { *draws = telemetry().last_draws; }
    if !uploads.is_null() { *uploads = telemetry().last_uploads; }
}

#[no_mangle]
pub unsafe extern "C" fn glide_gl_bridge_scheduler_metrics(
    workers: *mut u64,
    queued: *mut u64,
    cpu_busy_milli: *mut u64,
    gpu_busy_milli: *mut u64,
) {
    if !workers.is_null() { *workers = telemetry().scheduler_workers; }
    if !queued.is_null() { *queued = telemetry().scheduler_queued; }
    if !cpu_busy_milli.is_null() { *cpu_busy_milli = (telemetry().scheduler_cpu_busy * 1000.0) as u64; }
    if !gpu_busy_milli.is_null() { *gpu_busy_milli = (telemetry().scheduler_gpu_busy * 1000.0) as u64; }
}

#[no_mangle]
pub unsafe extern "system" fn glide_gl_bridge_scheduler_tick() {
    refresh_scheduler_budget();
}

#[no_mangle]
pub unsafe extern "system" fn glFinish() {
    ensure_symbols();
    if !vulkan_mode() { if let Some(real) = REAL_FINISH { real(); } }
}

#[no_mangle]
pub unsafe extern "system" fn glXSwapBuffers(display: *mut c_void, drawable: u64) {
    ensure_symbols();
    if STATE.commands.is_empty() {
        if let Some(real) = REAL_GLX_SWAP_BUFFERS { real(display, drawable); }
        return;
    }

    let presented = flush_vulkan_frame(display, drawable);
    if !presented {
        if let Some(real) = REAL_GLX_SWAP_BUFFERS { real(display, drawable); }
    }
}
