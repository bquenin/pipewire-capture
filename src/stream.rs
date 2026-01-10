//! PipeWire stream capture.
//!
//! This module handles capturing video frames from a PipeWire stream.

use crate::error::CaptureError;
use crate::gpu::{spa_format_to_drm, GpuFrameImporter};
use numpy::{PyArray3, PyArrayMethods};
use parking_lot::Mutex;
use pipewire::{
    context::Context,
    main_loop::MainLoop,
    properties::properties,
    spa::{
        pod::builder::{builder_add, Builder},
        sys as spa_sys,
        utils::Id,
    },
    stream::{Stream, StreamFlags, StreamListener, StreamRef, StreamState},
};
use pyo3::prelude::*;
use std::{
    mem::size_of,
    os::fd::{FromRawFd, OwnedFd, RawFd},
    sync::{mpsc, Arc},
    thread::JoinHandle,
    time::Instant,
};
use tracing::{debug, error, info, warn};

// DRM syncobj support for explicit sync
use std::fs::OpenOptions;
use std::os::fd::BorrowedFd;
use std::sync::OnceLock;

/// Global DRM render node fd for syncobj operations
static DRM_FD: OnceLock<OwnedFd> = OnceLock::new();

/// Get or open the DRM render node for syncobj operations
fn get_drm_fd() -> Option<BorrowedFd<'static>> {
    let fd = DRM_FD.get_or_init(|| {
        // Try common render node paths
        for path in &["/dev/dri/renderD128", "/dev/dri/renderD129"] {
            if let Ok(file) = OpenOptions::new().read(true).write(true).open(path) {
                use std::os::fd::IntoRawFd;
                let raw_fd = file.into_raw_fd();
                return unsafe { OwnedFd::from_raw_fd(raw_fd) };
            }
        }
        // Return an invalid fd that will cause syncobj operations to fail gracefully
        unsafe { OwnedFd::from_raw_fd(-1) }
    });

    use std::os::fd::AsRawFd;
    if fd.as_raw_fd() < 0 {
        None
    } else {
        // Safety: The DRM_FD is stored in a static OnceLock, so it lives for 'static
        Some(unsafe { BorrowedFd::borrow_raw(fd.as_raw_fd()) })
    }
}

// SPA constants for explicit sync
const SPA_TYPE_OBJECT_PARAM_META: u32 = spa_sys::SPA_TYPE_OBJECT_ParamMeta;
const SPA_PARAM_META: u32 = spa_sys::SPA_PARAM_Meta;
const SPA_PARAM_META_TYPE: u32 = spa_sys::SPA_PARAM_META_type;
const SPA_PARAM_META_SIZE: u32 = spa_sys::SPA_PARAM_META_size;
const SPA_META_SYNC_TIMELINE: u32 = spa_sys::SPA_META_SyncTimeline;

/// Shared state between Python thread and PipeWire thread.
struct SharedState {
    /// Latest frame data (BGRA, height * width * 4 bytes).
    frame_buffer: Option<Vec<u8>>,
    /// Frame width in pixels.
    width: u32,
    /// Frame height in pixels.
    height: u32,
    /// Stream has ended (window closed, error, or user stop).
    stream_ended: bool,
    /// Window was closed by the user.
    window_closed: bool,
    /// Last capture timestamp for throttling.
    last_capture_time: Instant,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            frame_buffer: None,
            width: 0,
            height: 0,
            stream_ended: false,
            window_closed: false,
            last_capture_time: Instant::now(),
        }
    }
}

/// Commands sent to the PipeWire thread.
enum Command {
    Stop,
}

/// User data passed to stream callbacks.
struct StreamUserData {
    shared: Arc<Mutex<SharedState>>,
    capture_interval: f64,
    weak_mainloop: pipewire::main_loop::WeakMainLoop,
    /// GPU frame importer for DMA buffer handling.
    gpu_importer: GpuFrameImporter,
    /// Current video format (SPA format code).
    format: u32,
    /// Current stride in bytes.
    stride: u32,
}

/// PipeWire-based video capture stream.
///
/// Captures frames from a PipeWire stream obtained via the portal.
/// Frames are returned as numpy arrays in BGRA format.
#[pyclass]
pub struct CaptureStream {
    /// File descriptor for PipeWire connection (consumed on start).
    fd: Option<OwnedFd>,
    /// Node ID for the capture stream.
    node_id: u32,
    /// Target interval between captures in seconds.
    capture_interval: f64,
    /// Whether the stream has been started.
    running: bool,
    /// Shared state with PipeWire thread.
    shared: Arc<Mutex<SharedState>>,
    /// Handle to the PipeWire thread.
    thread_handle: Option<JoinHandle<()>>,
    /// Sender for commands to PipeWire thread.
    command_tx: Option<mpsc::Sender<Command>>,
}

#[pymethods]
impl CaptureStream {
    /// Create a new capture stream.
    ///
    /// Args:
    ///     fd: PipeWire file descriptor from portal.
    ///     node_id: PipeWire node ID for the stream.
    ///     width: Initial width from portal (used if format negotiation doesn't provide it).
    ///     height: Initial height from portal (used if format negotiation doesn't provide it).
    ///     capture_interval: Target interval between frames in seconds.
    #[new]
    #[pyo3(signature = (fd, node_id, width, height, capture_interval=0.25))]
    pub fn new(
        fd: i32,
        node_id: u32,
        width: u32,
        height: u32,
        capture_interval: f64,
    ) -> PyResult<Self> {
        let owned_fd = duplicate_fd(fd)?;

        debug!(
            fd,
            node_id, width, height, capture_interval, "Creating CaptureStream"
        );

        Ok(Self {
            fd: Some(owned_fd),
            node_id,
            capture_interval,
            running: false,
            shared: Arc::new(Mutex::new(SharedState {
                width,
                height,
                ..SharedState::default()
            })),
            thread_handle: None,
            command_tx: None,
        })
    }

    /// Start capturing frames from the stream.
    pub fn start(&mut self) -> PyResult<()> {
        if self.running {
            return Err(CaptureError::AlreadyStarted.into());
        }

        let fd = self.fd.take().ok_or_else(|| {
            CaptureError::PipeWire("File descriptor already consumed".to_string())
        })?;

        let (tx, rx) = mpsc::channel();
        let shared = Arc::clone(&self.shared);
        let node_id = self.node_id;
        let capture_interval = self.capture_interval;

        debug!(node_id, "Starting PipeWire thread");

        let handle = std::thread::Builder::new()
            .name("pipewire-capture".into())
            .spawn(move || {
                pipewire_thread(fd, node_id, capture_interval, shared, rx);
            })
            .map_err(|e| CaptureError::ThreadSpawnFailed(e.to_string()))?;

        self.thread_handle = Some(handle);
        self.command_tx = Some(tx);
        self.running = true;

        info!(node_id, "Capture stream started");
        Ok(())
    }

    /// Get the latest captured frame.
    ///
    /// Returns a numpy array of shape (height, width, 4) in BGRA format,
    /// or None if no frame is available yet.
    pub fn get_frame<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyArray3<u8>>>> {
        if !self.running {
            return Ok(None);
        }

        // Release GIL while waiting for lock
        let (frame_data, width, height) = py.allow_threads(|| {
            let shared = self.shared.lock();
            (shared.frame_buffer.clone(), shared.width, shared.height)
        });

        match frame_data {
            Some(data) if width > 0 && height > 0 => {
                let expected = (height * width * 4) as usize;
                if data.len() != expected {
                    return Err(CaptureError::FrameSizeMismatch {
                        expected,
                        actual: data.len(),
                    }
                    .into());
                }

                // Create numpy array with shape (height, width, 4)
                let array = numpy::PyArray1::from_vec_bound(py, data);
                let array = array.reshape([height as usize, width as usize, 4])?;
                Ok(Some(array))
            }
            _ => Ok(None),
        }
    }

    /// Check if the captured window has been closed.
    pub fn is_window_closed(&self) -> bool {
        self.shared.lock().window_closed
    }

    /// Stop capturing and release resources.
    pub fn stop(&mut self) -> PyResult<()> {
        if !self.running {
            return Ok(());
        }

        debug!("Stopping capture stream");

        // Send stop command
        if let Some(tx) = self.command_tx.take() {
            let _ = tx.send(Command::Stop);
        }

        // Join thread (release GIL during potentially long wait)
        if let Some(handle) = self.thread_handle.take() {
            Python::with_gil(|py| {
                py.allow_threads(|| {
                    if let Err(e) = handle.join() {
                        error!("PipeWire thread panicked: {:?}", e);
                    }
                });
            });
        }

        self.running = false;
        info!("Capture stream stopped");
        Ok(())
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        if self.running {
            debug!("CaptureStream dropped while running, cleaning up");
            // Best effort cleanup
            if let Some(tx) = self.command_tx.take() {
                let _ = tx.send(Command::Stop);
            }
            if let Some(handle) = self.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

/// Duplicate a file descriptor.
fn duplicate_fd(fd: i32) -> PyResult<OwnedFd> {
    let dup_fd = unsafe { libc::dup(fd) };
    if dup_fd < 0 {
        return Err(CaptureError::PipeWire(format!(
            "Failed to duplicate fd: {}",
            std::io::Error::last_os_error()
        ))
        .into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(dup_fd) })
}

/// Main function for the PipeWire thread.
fn pipewire_thread(
    fd: OwnedFd,
    node_id: u32,
    capture_interval: f64,
    shared: Arc<Mutex<SharedState>>,
    command_rx: mpsc::Receiver<Command>,
) {
    debug!(node_id, "PipeWire thread starting");

    // Initialize GPU importer first (requires EGL)
    let gpu_importer = match GpuFrameImporter::new() {
        Ok(gpu) => gpu,
        Err(e) => {
            error!("Failed to initialize GPU importer: {}", e);
            shared.lock().stream_ended = true;
            return;
        }
    };

    // Initialize PipeWire
    pipewire::init();

    let mainloop = match MainLoop::new(None) {
        Ok(ml) => ml,
        Err(e) => {
            error!("Failed to create PipeWire main loop: {}", e);
            shared.lock().stream_ended = true;
            return;
        }
    };

    let context = match Context::new(&mainloop) {
        Ok(ctx) => ctx,
        Err(e) => {
            error!("Failed to create PipeWire context: {}", e);
            shared.lock().stream_ended = true;
            return;
        }
    };

    let core = match context.connect_fd(fd, None) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to connect to PipeWire via fd: {}", e);
            shared.lock().stream_ended = true;
            return;
        }
    };

    info!(node_id, "Connected to PipeWire");

    // Setup stream
    let result = setup_stream(
        &core,
        node_id,
        &mainloop,
        Arc::clone(&shared),
        capture_interval,
        gpu_importer,
    );

    let (_stream, _listener) = match result {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to setup stream: {}", e);
            shared.lock().stream_ended = true;
            return;
        }
    };

    // Add timer to check for stop commands
    let weak_ml = mainloop.downgrade();
    let timer_callback = move |_expirations: u64| {
        if command_rx.try_recv().is_ok() {
            debug!("Received stop command");
            if let Some(ml) = weak_ml.upgrade() {
                ml.quit();
            }
        }
    };
    let timer = mainloop.loop_().add_timer(timer_callback);
    timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    debug!("Running PipeWire main loop");
    mainloop.run();
    debug!("PipeWire main loop exited");

    shared.lock().stream_ended = true;
}

/// Setup the PipeWire stream with callbacks.
fn setup_stream(
    core: &pipewire::core::Core,
    node_id: u32,
    mainloop: &MainLoop,
    shared: Arc<Mutex<SharedState>>,
    capture_interval: f64,
    gpu_importer: GpuFrameImporter,
) -> Result<(Stream, StreamListener<StreamUserData>), CaptureError> {
    let props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Video",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Screen",
    };

    let stream = Stream::new(core, "pipewire-capture", props)
        .map_err(|e| CaptureError::PipeWire(e.to_string()))?;

    let user_data = StreamUserData {
        shared,
        capture_interval,
        weak_mainloop: mainloop.downgrade(),
        gpu_importer,
        format: 0,
        stride: 0,
    };

    let listener = stream
        .add_local_listener_with_user_data(user_data)
        .state_changed(|_stream, data, old, new| {
            on_state_changed(data, old, new);
        })
        .param_changed(|stream, data, id, param| {
            on_param_changed(stream, data, id, param);
        })
        .process(|stream, data| {
            on_process(stream, data);
        })
        .register()
        .map_err(|e| CaptureError::PipeWire(e.to_string()))?;

    // Build format params with framerate to request high FPS
    use pipewire::spa::pod::{serialize::PodSerializer, Value};

    let obj = pipewire::spa::pod::object!(
        pipewire::spa::utils::SpaTypes::ObjectParamFormat,
        pipewire::spa::param::ParamType::EnumFormat,
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::MediaType,
            Id,
            pipewire::spa::param::format::MediaType::Video
        ),
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pipewire::spa::param::format::MediaSubtype::Raw
        ),
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            pipewire::spa::param::video::VideoFormat::BGRx,
            pipewire::spa::param::video::VideoFormat::BGRx,
            pipewire::spa::param::video::VideoFormat::BGRA,
            pipewire::spa::param::video::VideoFormat::RGBx,
            pipewire::spa::param::video::VideoFormat::RGBA,
        ),
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pipewire::spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            pipewire::spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            pipewire::spa::utils::Rectangle {
                width: 4096,
                height: 4096
            }
        ),
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            pipewire::spa::utils::Fraction { num: 60, denom: 1 },
            pipewire::spa::utils::Fraction { num: 0, denom: 1 },
            pipewire::spa::utils::Fraction {
                num: 1000,
                denom: 1
            }
        ),
    );

    let values: Vec<u8> =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .map_err(|e| {
                CaptureError::PipeWire(format!("Failed to serialize format params: {:?}", e))
            })?
            .0
            .into_inner();

    let mut params = [pipewire::spa::pod::Pod::from_bytes(&values).unwrap()];

    // Connect to the node with format params
    // MAP_BUFFERS gives us memory-mapped buffers for efficient CPU access
    stream
        .connect(
            pipewire::spa::utils::Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| CaptureError::PipeWire(e.to_string()))?;

    debug!(node_id, "Stream connected");

    Ok((stream, listener))
}

/// Handle stream state changes.
fn on_state_changed(data: &mut StreamUserData, old: StreamState, new: StreamState) {
    debug!(?old, ?new, "Stream state changed");

    match &new {
        StreamState::Error(msg) => {
            error!("Stream error: {}", msg);
            let mut shared = data.shared.lock();
            shared.stream_ended = true;
            shared.window_closed = true;
            if let Some(ml) = data.weak_mainloop.upgrade() {
                ml.quit();
            }
        }
        StreamState::Unconnected => {
            // Only treat as window closed if we were previously streaming
            if matches!(old, StreamState::Streaming | StreamState::Paused) {
                info!("Stream disconnected (window closed)");
                let mut shared = data.shared.lock();
                shared.stream_ended = true;
                shared.window_closed = true;
                if let Some(ml) = data.weak_mainloop.upgrade() {
                    ml.quit();
                }
            }
        }
        StreamState::Streaming => {
            info!("Stream now streaming");
        }
        StreamState::Paused => {
            debug!("Stream paused");
        }
        StreamState::Connecting => {
            debug!("Stream connecting");
        }
    }
}

/// Handle format parameter changes.
fn on_param_changed(
    stream: &StreamRef,
    data: &mut StreamUserData,
    id: u32,
    param: Option<&pipewire::spa::pod::Pod>,
) {
    // SPA_PARAM_Format = 4 (not 3, which is SPA_PARAM_EnumFormat)
    const SPA_PARAM_FORMAT: u32 = spa_sys::SPA_PARAM_Format;

    if id != SPA_PARAM_FORMAT {
        return;
    }

    let Some(param) = param else {
        return;
    };

    // Parse video format using spa_sys
    let info = unsafe {
        let mut video_info: spa_sys::spa_video_info_raw = std::mem::zeroed();
        let result = spa_sys::spa_format_video_raw_parse(param.as_raw_ptr(), &mut video_info);
        if result < 0 {
            warn!("Failed to parse video format");
            return;
        }
        video_info
    };

    // Calculate stride (row size in bytes)
    // For 32-bit formats (BGRA, etc), stride = width * 4
    let stride = info.size.width * 4;

    debug!(
        width = info.size.width,
        height = info.size.height,
        format = info.format,
        stride,
        "Video format negotiated"
    );

    // Store format info for GPU import
    data.format = info.format;
    data.stride = stride;

    {
        let mut shared = data.shared.lock();
        shared.width = info.size.width;
        shared.height = info.size.height;
    }

    // Request SyncTimeline metadata for explicit sync (NVIDIA fix)
    // This allows us to wait for the compositor to finish writing before reading
    request_sync_timeline_metadata(stream);
}

/// Request SyncTimeline metadata from PipeWire for explicit synchronization.
///
/// This is needed for NVIDIA GPUs which don't support implicit buffer sync.
/// When the compositor supports explicit sync, it will provide syncobj fds
/// that we can wait on before reading the DmaBuf.
fn request_sync_timeline_metadata(stream: &StreamRef) {
    let mut pod_data = Vec::with_capacity(256);
    let mut builder = Builder::new(&mut pod_data);

    // Build SPA_PARAM_Meta object requesting SyncTimeline metadata
    // Equivalent to C code:
    //   spa_pod_builder_push_object(&b, &f, SPA_TYPE_OBJECT_ParamMeta, SPA_PARAM_Meta);
    //   spa_pod_builder_add(&b,
    //       SPA_PARAM_META_type, SPA_POD_Id(SPA_META_SyncTimeline),
    //       SPA_PARAM_META_size, SPA_POD_Int(sizeof(struct spa_meta_sync_timeline)),
    //       0);
    let result = builder_add!(
        &mut builder,
        Object(SPA_TYPE_OBJECT_PARAM_META, SPA_PARAM_META) {
            SPA_PARAM_META_TYPE => Id(Id(SPA_META_SYNC_TIMELINE)),
            SPA_PARAM_META_SIZE => Int(size_of::<spa_sys::spa_meta_sync_timeline>() as i32),
        }
    );

    if let Err(e) = result {
        warn!("Failed to build SyncTimeline meta param: {:?}", e);
        return;
    }

    // Get the pod from the builder
    let pod = unsafe {
        let raw_pod = spa_sys::spa_pod_builder_deref(builder.as_raw_ptr(), 0);
        if raw_pod.is_null() {
            return;
        }
        &*(raw_pod as *const pipewire::spa::pod::Pod)
    };

    // Update stream params to request SyncTimeline metadata
    let mut params = [pod];
    if let Err(e) = stream.update_params(&mut params) {
        warn!("Failed to request SyncTimeline metadata: {:?}", e);
    } else {
        info!("Requested SyncTimeline metadata for explicit sync");
    }
}

/// Process incoming video frames.
fn on_process(stream: &StreamRef, data: &mut StreamUserData) {
    let now = Instant::now();

    // Use raw buffer API to access spa_buffer for metadata lookup
    let pw_buffer = unsafe { stream.dequeue_raw_buffer() };
    if pw_buffer.is_null() {
        return;
    }

    // Get the spa_buffer from pw_buffer
    let spa_buffer = unsafe { (*pw_buffer).buffer };
    if spa_buffer.is_null() {
        unsafe { stream.queue_raw_buffer(pw_buffer) };
        return;
    }

    // Check throttling
    let should_capture = {
        let shared = data.shared.lock();
        now.duration_since(shared.last_capture_time).as_secs_f64() >= data.capture_interval
    };

    if !should_capture {
        unsafe { stream.queue_raw_buffer(pw_buffer) };
        return;
    }

    // Get buffer data array
    let n_datas = unsafe { (*spa_buffer).n_datas };
    if n_datas == 0 {
        unsafe { stream.queue_raw_buffer(pw_buffer) };
        return;
    }

    let datas_ptr = unsafe { (*spa_buffer).datas };
    if datas_ptr.is_null() {
        unsafe { stream.queue_raw_buffer(pw_buffer) };
        return;
    }

    // Get dimensions from shared state
    let (width, height) = {
        let shared = data.shared.lock();
        (shared.width, shared.height)
    };

    if width == 0 || height == 0 {
        warn!("Frame dimensions not yet known");
        unsafe { stream.queue_raw_buffer(pw_buffer) };
        return;
    }

    // Check if this is a DMA buffer
    let first_data = unsafe { &*datas_ptr };
    let data_type_raw = first_data.type_;

    // SPA_DATA_DmaBuf = 3
    const SPA_DATA_DMABUF: u32 = 3;
    if data_type_raw != SPA_DATA_DMABUF {
        // For non-DmaBuf, try direct memory access
        if !first_data.data.is_null() && !first_data.chunk.is_null() {
            let chunk = unsafe { &*first_data.chunk };
            let size = chunk.size as usize;
            let offset = chunk.offset as usize;
            if size > 0 {
                let frame_slice = unsafe {
                    std::slice::from_raw_parts(first_data.data.add(offset) as *const u8, size)
                };
                let mut shared = data.shared.lock();
                shared.frame_buffer = Some(frame_slice.to_vec());
                shared.last_capture_time = now;
            }
        }
        unsafe { stream.queue_raw_buffer(pw_buffer) };
        return;
    }

    // DMA buffer - check for SyncTimeline metadata and wait if present
    wait_on_sync_timeline(spa_buffer, datas_ptr, n_datas);

    // Get DmaBuf fd
    let dmabuf_fd = first_data.fd as i32;

    if dmabuf_fd < 0 {
        warn!("Invalid DmaBuf fd: {}", dmabuf_fd);
        unsafe { stream.queue_raw_buffer(pw_buffer) };
        return;
    }

    // Convert SPA format to DRM format
    // For portal DmaBuf streams, if format is unknown, try common defaults
    let drm_format = if data.format != 0 {
        match spa_format_to_drm(data.format) {
            Some(fmt) => fmt,
            None => {
                warn!("Unknown SPA format: {}", data.format);
                unsafe { stream.queue_raw_buffer(pw_buffer) };
                return;
            }
        }
    } else {
        // Format not received via param_changed, try common default
        // Most compositors use XRGB8888 (BGRx) or ARGB8888 (BGRA)
        use crate::gpu::DRM_FORMAT_XRGB8888;
        DRM_FORMAT_XRGB8888
    };

    // Use calculated stride or fallback to width * 4
    let stride = if data.stride != 0 {
        data.stride
    } else {
        width * 4
    };

    // Import frame via EGL
    match data
        .gpu_importer
        .import_frame(dmabuf_fd, width, height, stride, drm_format)
    {
        Ok(frame_vec) => {
            let mut shared = data.shared.lock();
            shared.frame_buffer = Some(frame_vec);
            shared.last_capture_time = now;
        }
        Err(e) => {
            error!("GPU import failed: {}", e);
        }
    }

    // Queue buffer back to PipeWire
    unsafe { stream.queue_raw_buffer(pw_buffer) };
}

/// Wait on explicit sync acquire_point if SyncTimeline metadata is present.
///
/// When the compositor supports explicit sync, it provides:
/// - SPA_META_SyncTimeline metadata with acquire_point and release_point
/// - Extra data blocks: datas[1] = acquire syncobj fd, datas[2] = release syncobj fd
///
/// We wait on the acquire_point before reading the DmaBuf to ensure the
/// compositor has finished writing to it.
fn wait_on_sync_timeline(
    spa_buffer: *mut spa_sys::spa_buffer,
    datas_ptr: *mut spa_sys::spa_data,
    n_datas: u32,
) {
    // Find SyncTimeline metadata
    let stl = unsafe {
        spa_sys::spa_buffer_find_meta_data(
            spa_buffer,
            SPA_META_SYNC_TIMELINE,
            size_of::<spa_sys::spa_meta_sync_timeline>(),
        ) as *const spa_sys::spa_meta_sync_timeline
    };

    if stl.is_null() {
        // No SyncTimeline metadata - compositor doesn't support explicit sync
        // Fall back to implicit sync (which may be slow on NVIDIA)
        return;
    }

    let acquire_point = unsafe { (*stl).acquire_point };
    if acquire_point == 0 {
        // No acquire point set
        return;
    }

    // Check if we have the acquire syncobj fd (should be in datas[1])
    if n_datas < 2 {
        return;
    }

    let acquire_data = unsafe { &*datas_ptr.add(1) };

    // Check if datas[1] is actually a SyncObj (type=5)
    const SPA_DATA_SYNCOBJ: u32 = 5;
    if acquire_data.type_ != SPA_DATA_SYNCOBJ {
        // Compositor doesn't support explicit sync for this buffer
        return;
    }

    let acquire_fd = acquire_data.fd as RawFd;
    if acquire_fd < 0 {
        return;
    }

    let wait_start = Instant::now();

    // Get DRM device fd for syncobj operations
    let Some(drm_fd) = get_drm_fd() else {
        return;
    };

    // Import the syncobj fd as a local handle
    let syncobj_fd = unsafe { BorrowedFd::borrow_raw(acquire_fd) };
    let handle = match drm_ffi::syncobj::fd_to_handle(drm_fd, syncobj_fd, false) {
        Ok(result) => result.handle,
        Err(_) => return,
    };

    // Wait on the timeline point (5 second timeout from now)
    // timeout_nsec is an absolute deadline in CLOCK_MONOTONIC nanoseconds
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    let now_nsec = ts.tv_sec * 1_000_000_000 + ts.tv_nsec;
    let timeout_nsec = now_nsec + 5_000_000_000i64; // 5 seconds from now

    let handles = [handle];
    let points = [acquire_point];

    match drm_ffi::syncobj::timeline_wait(
        drm_fd,
        &handles,
        &points,
        timeout_nsec,
        true,  // wait_all
        true,  // wait_for_submit
        false, // wait_available
    ) {
        Ok(_) => {
            let elapsed = wait_start.elapsed();
            info!("SyncTimeline acquired in {:?}", elapsed);
        }
        Err(e) => {
            warn!("SyncTimeline timeline_wait error: {}", e);
        }
    }

    // Clean up the imported handle
    let _ = drm_ffi::syncobj::destroy(drm_fd, handle);
}
