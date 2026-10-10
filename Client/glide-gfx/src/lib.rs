
use std::collections::{HashMap, HashSet};

/// Core graphics command layer for Glide.
/// The command stream is intentionally backend-neutral: we record GPU-like operations in a
/// platform-independent form and then optimize redundant state changes before handing them to a
/// concrete backend such as Vulkan or an OpenGL bridge. This keeps the translation layer simple and
/// makes the command pipeline easier to reason about when debugging rendering issues.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId(u32);

impl ResourceId {
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Primitive {
    Points,
    Lines,
    Triangles,
    TriangleStrip,
    TriangleFan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferUsage {
    Vertex,
    Index,
    Uniform,
    Storage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    Rgba8,
    Bgra8,
    Rgb8,
    Depth24Stencil8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    Disabled,
    Alpha,
    Additive,
}

impl Default for BlendMode {
    fn default() -> Self {
        Self::Disabled
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VertexAttributeType {
    Float32,
    Float32x2,
    Float32x3,
    Float32x4,
    Uint8x4Norm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexAttribute {
    pub location: u32,
    pub offset: u32,
    pub ty: VertexAttributeType,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct VertexLayout {
    pub stride: u32,
    pub attributes: [Option<VertexAttribute>; 8],
}

impl VertexLayout {
    pub const fn empty() -> Self {
        Self { stride: 0, attributes: [None; 8] }
    }

    pub fn with_attribute(mut self, slot: usize, attribute: VertexAttribute) -> Self {
        if slot < self.attributes.len() {
            self.attributes[slot] = Some(attribute);
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexType {
    Uint8,
    Uint16,
    Uint32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendFactor {
    Zero,
    One,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    SrcColor,
    DstColor,
    OneMinusSrcColor,
    OneMinusDstColor,
}

/// A backend-neutral rendering command. These are collected by the GL bridge and later reduced to
/// the minimal state changes needed by the active renderer.
#[derive(Clone, Debug)]
pub enum Command {
    CreateBuffer { id: ResourceId, usage: BufferUsage, size: usize },
    UploadBuffer { id: ResourceId, offset: usize, data: Vec<u8> },
    CreateTexture { id: ResourceId, width: u32, height: u32, format: TextureFormat },
    UploadTexture { id: ResourceId, x: u32, y: u32, width: u32, height: u32, data: Vec<u8> },
    BindVertexBuffer(ResourceId),
    BindIndexBuffer(ResourceId),
    BindTexture { slot: u32, texture: ResourceId },
    SetVertexLayout(VertexLayout),
    SetIndexType(IndexType),
    SetBlend(BlendMode),
    SetBlendFactors { src: BlendFactor, dst: BlendFactor },
    SetDepthTest(bool),
    SetShaderProgram(ResourceId),
    Draw { primitive: Primitive, first: u32, count: u32 },
    DrawIndexed { primitive: Primitive, count: u32, index_offset: u32 },
    Present,
}

/// A FIFO of rendering commands. The command buffer supports a small optimization pass that drops
/// redundant state changes while preserving the last effective configuration.
#[derive(Default)]
pub struct CommandBuffer {
    commands: Vec<Command>,
}

impl CommandBuffer {
    pub const fn new() -> Self { Self { commands: Vec::new() } }

    #[inline]
    pub fn push(&mut self, command: Command) { self.commands.push(command); }

    #[inline]
    pub fn extend<I: IntoIterator<Item = Command>>(&mut self, iter: I) {
        self.commands.extend(iter);
    }

    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.commands.reserve(additional);
    }

    pub fn len(&self) -> usize { self.commands.len() }

    pub fn is_empty(&self) -> bool { self.commands.is_empty() }

    pub fn commands(&self) -> &[Command] { &self.commands }

    pub fn clear(&mut self) { self.commands.clear(); }

    /// Remove redundant state changes while keeping the final state of the render pass intact.
    pub fn optimize(&mut self) {
        let mut state = StateTracker::default();
        let mut optimized = Vec::with_capacity(self.commands.len());
        let mut stale_ids = HashSet::new();

        for command in std::mem::take(&mut self.commands) {
            match command {
                Command::CreateBuffer { id, .. } | Command::CreateTexture { id, .. } => {
                    stale_ids.insert(id);
                    state.invalidate_resource(id);
                    optimized.push(command);
                }
                Command::SetBlend(mode) => {
                    if state.set_blend(mode) {
                        optimized.push(Command::SetBlend(mode));
                    }
                }
                Command::SetDepthTest(enabled) => {
                    if state.set_depth(enabled) {
                        optimized.push(Command::SetDepthTest(enabled));
                    }
                }
                Command::BindVertexBuffer(id) => {
                    if state.set_vertex(id) {
                        optimized.push(Command::BindVertexBuffer(id));
                    }
                }
                Command::BindIndexBuffer(id) => {
                    if state.set_index(id) {
                        optimized.push(Command::BindIndexBuffer(id));
                    }
                }
                Command::SetVertexLayout(layout) => {
                    if state.set_layout(layout) {
                        optimized.push(Command::SetVertexLayout(layout));
                    }
                }
                Command::SetIndexType(index_type) => {
                    if state.set_index_type(index_type) {
                        optimized.push(Command::SetIndexType(index_type));
                    }
                }
                Command::SetShaderProgram(id) => {
                    if state.set_shader(id) {
                        optimized.push(Command::SetShaderProgram(id));
                    }
                }
                Command::BindTexture { slot, texture } => {
                    if state.set_texture(slot, texture) {
                        optimized.push(Command::BindTexture { slot, texture });
                    }
                }
                _ => optimized.push(command),
            }
        }

        self.commands = optimized
            .into_iter()
            .filter(|command| !binding_for_stale_resource(command, &stale_ids))
            .collect();
    }
}

#[inline]
fn binding_for_stale_resource(command: &Command, stale_ids: &HashSet<ResourceId>) -> bool {
    match command {
        Command::BindVertexBuffer(resource) => stale_ids.contains(resource),
        Command::BindIndexBuffer(resource) => stale_ids.contains(resource),
        Command::SetShaderProgram(resource) => stale_ids.contains(resource),
        Command::BindTexture { texture, .. } => stale_ids.contains(texture),
        _ => false,
    }
}

/// Tracks the live state of a command stream while it is being de-duplicated.
#[derive(Default)]
struct StateTracker {
    blend: Option<BlendMode>,
    depth: Option<bool>,
    vertex: Option<ResourceId>,
    index: Option<ResourceId>,
    layout: Option<VertexLayout>,
    index_type: Option<IndexType>,
    shader: Option<ResourceId>,
    textures: HashMap<u32, ResourceId>,
}

impl StateTracker {
    #[inline]
    fn set_blend(&mut self, value: BlendMode) -> bool {
        if self.blend == Some(value) { false } else { self.blend = Some(value); true }
    }

    #[inline]
    fn set_depth(&mut self, value: bool) -> bool {
        if self.depth == Some(value) { false } else { self.depth = Some(value); true }
    }

    #[inline]
    fn set_vertex(&mut self, value: ResourceId) -> bool {
        if self.vertex == Some(value) { false } else { self.vertex = Some(value); true }
    }

    #[inline]
    fn set_index(&mut self, value: ResourceId) -> bool {
        if self.index == Some(value) { false } else { self.index = Some(value); true }
    }

    #[inline]
    fn set_layout(&mut self, value: VertexLayout) -> bool {
        if self.layout == Some(value) { false } else { self.layout = Some(value); true }
    }

    #[inline]
    fn set_index_type(&mut self, value: IndexType) -> bool {
        if self.index_type == Some(value) { false } else { self.index_type = Some(value); true }
    }

    #[inline]
    fn set_shader(&mut self, value: ResourceId) -> bool {
        if self.shader == Some(value) { false } else { self.shader = Some(value); true }
    }

    fn invalidate_resource(&mut self, id: ResourceId) {
        // Resource IDs are backend-neutral, so a reused ID must clear any stale references from the
        // current frame even if the original binding was for a different kind of resource.
        if self.vertex == Some(id) { self.vertex = None; }
        if self.index == Some(id) { self.index = None; }
        if self.shader == Some(id) { self.shader = None; }
        self.textures.retain(|_, texture| *texture != id);
    }

    fn set_texture(&mut self, slot: u32, value: ResourceId) -> bool {
        if self.textures.get(&slot).copied() == Some(value) {
            false
        } else {
            self.textures.insert(slot, value);
            true
        }
    }
}

pub trait Backend {
    type Error;

    fn submit(&mut self, commands: &[Command]) -> Result<(), Self::Error>;
    fn present(&mut self) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redundant_state_is_removed() {
        let mut commands = CommandBuffer::new();
        commands.push(Command::SetDepthTest(true));
        commands.push(Command::SetDepthTest(true));
        commands.push(Command::SetBlend(BlendMode::Alpha));
        commands.push(Command::SetBlend(BlendMode::Alpha));
        commands.push(Command::Present);

        commands.optimize();

        assert_eq!(commands.len(), 3);
    }

    #[test]
    fn resource_ids_are_stable() {
        let id = ResourceId::from_raw(42);
        assert_eq!(id.raw(), 42);
    }

    #[test]
    fn reused_resource_ids_clear_stale_state() {
        let mut commands = CommandBuffer::new();
        commands.push(Command::SetShaderProgram(ResourceId::from_raw(9)));
        commands.push(Command::BindTexture { slot: 0, texture: ResourceId::from_raw(9) });
        commands.push(Command::CreateBuffer {
            id: ResourceId::from_raw(9),
            usage: BufferUsage::Vertex,
            size: 64,
        });
        commands.push(Command::Present);

        commands.optimize();

        assert!(!commands.commands().iter().any(|cmd| matches!(
            cmd,
            Command::SetShaderProgram(id) if id.raw() == 9
        )));
        assert!(!commands.commands().iter().any(|cmd| matches!(
            cmd,
            Command::BindTexture { slot: _, texture } if texture.raw() == 9
        )));
        assert!(commands.commands().iter().any(|cmd| matches!(cmd, Command::Present)));
    }
}


#[cfg(feature = "vulkan")]
pub mod vulkan;

pub mod scheduler {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex,
    };
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    enum Message {
        Run(Box<dyn FnOnce() + Send + 'static>),
        Stop,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Workload {
        Heavy,
        Normal,
        Idle,
    }

    #[derive(Clone, Copy, Debug)]
    pub struct SchedulerSnapshot {
        pub workers: usize,
        pub queued: usize,
        pub cpu_busy: f32,
        pub gpu_busy: f32,
        pub frame_ms: f32,
        pub workload: Workload,
    }

    pub struct RenderScheduler {
        tx: Sender<Message>,
        queued: Arc<AtomicUsize>,
        worker_count: Arc<AtomicUsize>,
        desired: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        handles: Vec<JoinHandle<()>>,
        frame_start: Instant,
        last_frame_ms: f32,
        cpu_busy: f32,
        gpu_busy: f32,
    }

    impl RenderScheduler {
        pub fn new(max_workers: usize) -> Self {
            let max_workers = max_workers.min(3);
            let (tx, rx) = mpsc::channel::<Message>();
            let rx = Arc::new(Mutex::new(rx));
            let queued = Arc::new(AtomicUsize::new(0));
            let worker_count = Arc::new(AtomicUsize::new(0));
            let desired = Arc::new(AtomicUsize::new(max_workers));
            let stop = Arc::new(AtomicBool::new(false));
            let mut handles = Vec::with_capacity(max_workers);

            for _ in 0..max_workers {
                let rx = Arc::clone(&rx);
                let queued = Arc::clone(&queued);
                let worker_count = Arc::clone(&worker_count);
                let desired = Arc::clone(&desired);
                let stop = Arc::clone(&stop);
                handles.push(thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let budget = desired.load(Ordering::Acquire);
                        let mut reserved = false;
                        loop {
                            let active = worker_count.load(Ordering::Acquire);
                            if active >= budget {
                                break;
                            }
                            if worker_count
                                .compare_exchange(
                                    active,
                                    active + 1,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                )
                                .is_ok()
                            {
                                reserved = true;
                                break;
                            }
                        }
                        if !reserved {
                            thread::yield_now();
                            thread::sleep(Duration::from_micros(250));
                            continue;
                        }

                        let message = {
                            let guard = rx.lock().expect("render scheduler queue poisoned");
                            guard.recv_timeout(Duration::from_millis(2))
                        };
                        match message {
                            Ok(Message::Run(task)) => {
                                queued.fetch_sub(1, Ordering::Relaxed);
                                task();
                                worker_count.fetch_sub(1, Ordering::Release);
                            }
                            Ok(Message::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                                worker_count.fetch_sub(1, Ordering::Release);
                                break;
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                worker_count.fetch_sub(1, Ordering::Release);
                            }
                        }
                    }
                }));
            }

            Self {
                tx,
                queued,
                worker_count,
                desired,
                stop,
                handles,
                frame_start: Instant::now(),
                last_frame_ms: 0.0,
                cpu_busy: 0.0,
                gpu_busy: 0.0,
            }
        }

        pub fn begin_frame(&mut self, cpu_busy: f32, gpu_busy: f32) {
            self.frame_start = Instant::now();
            self.cpu_busy = cpu_busy.clamp(0.0, 1.0);
            self.gpu_busy = gpu_busy.clamp(0.0, 1.0);
            self.desired.store(self.desired_workers(), Ordering::Release);
        }

        pub fn end_frame(&mut self) {
            self.last_frame_ms = self.frame_start.elapsed().as_secs_f32() * 1000.0;
        }

        pub fn workload(&self) -> Workload {
            if self.cpu_busy >= 0.90 {
                Workload::Heavy
            } else if self.cpu_busy >= 0.80 || self.gpu_busy >= 0.96 {
                Workload::Heavy
            } else if self.cpu_busy >= 0.50 || self.gpu_busy >= 0.90 {
                Workload::Normal
            } else {
                Workload::Idle
            }
        }

        pub fn desired_workers(&self) -> usize {
            if self.cpu_busy >= 0.90 {
                return 0;
            }
            match self.workload() {
                Workload::Heavy => 1,
                Workload::Normal => 2,
                Workload::Idle => 3,
            }
        }

        pub fn update_budget(&mut self, cpu_busy: f32, gpu_busy: f32) {
            self.cpu_busy = cpu_busy.clamp(0.0, 1.0);
            self.gpu_busy = gpu_busy.clamp(0.0, 1.0);
            self.desired.store(self.desired_workers(), Ordering::Release);
        }

        pub fn submit<F>(&self, task: F) -> bool
        where
            F: FnOnce() + Send + 'static,
        {
            if self.desired.load(Ordering::Acquire) == 0 {
                return false;
            }
            let queued = self.queued.fetch_add(1, Ordering::Relaxed) + 1;
            if queued > 128 {
                self.queued.fetch_sub(1, Ordering::Relaxed);
                return false;
            }
            if self.tx.send(Message::Run(Box::new(task))).is_err() {
                self.queued.fetch_sub(1, Ordering::Relaxed);
                return false;
            }
            true
        }

        pub fn snapshot(&self) -> SchedulerSnapshot {
            SchedulerSnapshot {
                workers: self.worker_count.load(Ordering::Relaxed),
                queued: self.queued.load(Ordering::Relaxed),
                cpu_busy: self.cpu_busy,
                gpu_busy: self.gpu_busy,
                frame_ms: self.last_frame_ms,
                workload: self.workload(),
            }
        }
    }

    impl Drop for RenderScheduler {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            for _ in &self.handles {
                let _ = self.tx.send(Message::Stop);
            }
            while let Some(handle) = self.handles.pop() {
                let _ = handle.join();
            }
        }
    }
}
