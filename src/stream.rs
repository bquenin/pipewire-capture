//! PipeWire stream capture.
//!
//! This module handles capturing video frames from a PipeWire stream.

use crate::error::CaptureError;
use numpy::{PyArray3, PyArrayMethods};
use parking_lot::Mutex;
use pipewire::{
    context::Context,
    main_loop::MainLoop,
    properties::properties,
    spa::sys as spa_sys,
    stream::{Stream, StreamFlags, StreamListener, StreamRef, StreamState},
};
use pyo3::prelude::*;
use std::{
    os::fd::{FromRawFd, OwnedFd},
    sync::{mpsc, Arc},
    thread::JoinHandle,
    time::Instant,
};
use tracing::{debug, error, info, warn};

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
    _stream: &StreamRef,
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

    debug!(
        width = info.size.width,
        height = info.size.height,
        format = info.format,
        "Video format negotiated"
    );

    {
        let mut shared = data.shared.lock();
        shared.width = info.size.width;
        shared.height = info.size.height;
    }
}

/// Process incoming video frames.
fn on_process(stream: &StreamRef, data: &mut StreamUserData) {
    let now = Instant::now();

    // Use raw buffer API to access spa_buffer
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

    // Check buffer type
    let first_data = unsafe { &*datas_ptr };
    let data_type = first_data.type_;

    // SPA_DATA_MemPtr = 2, SPA_DATA_DmaBuf = 3
    const SPA_DATA_MEMPTR: u32 = 2;
    const SPA_DATA_DMABUF: u32 = 3;

    match data_type {
        SPA_DATA_MEMPTR => {
            // Memory-mapped buffer - direct access
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
        }
        SPA_DATA_DMABUF => {
            // DmaBuf not supported - we request MAP_BUFFERS which should give us MemPtr
            error!(
                "Received DmaBuf buffer (type={}), but only MemPtr is supported. \
                 This may indicate an old PipeWire/compositor version. \
                 Please ensure you have PipeWire 0.3.30+ and a modern compositor (GNOME 3.36+, KDE 5.20+).",
                data_type
            );
        }
        _ => {
            warn!("Unknown buffer type: {}", data_type);
        }
    }

    // Queue buffer back to PipeWire
    unsafe { stream.queue_raw_buffer(pw_buffer) };
}
