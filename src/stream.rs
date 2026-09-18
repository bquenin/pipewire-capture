//! PipeWire stream capture.
//!
//! This module handles capturing video frames from a PipeWire stream.

use crate::error::CaptureError;
use crate::frame::{copy_frame, FrameFormat};
use numpy::{PyArray3, PyArrayMethods};
use parking_lot::Mutex;
use pipewire::{
    context::Context,
    main_loop::MainLoop,
    properties::properties,
    spa::sys as spa_sys,
    stream::{Stream, StreamFlags, StreamListener, StreamRef, StreamState},
};
use pyo3::{exceptions::PyValueError, prelude::*};
use std::{
    os::fd::{FromRawFd, OwnedFd},
    sync::{mpsc, Arc},
    thread::JoinHandle,
    time::Instant,
};
use tracing::{debug, error, info, warn};

/// Shared state between Python thread and PipeWire thread.
#[derive(Default)]
struct SharedState {
    /// Latest frame data (BGRA, height * width * 4 bytes).
    frame_buffer: Option<Vec<u8>>,
    /// Format negotiated with PipeWire, independent of portal size hints.
    format: Option<FrameFormat>,
    /// Stream has ended (window closed, error, or user stop).
    stream_ended: bool,
    /// Last capture timestamp for throttling.
    last_capture_time: Option<Instant>,
    /// Failure reported by the capture thread.
    error: Option<String>,
}

impl SharedState {
    fn fail(&mut self, message: String) {
        self.error = Some(message);
        self.stream_ended = true;
        self.frame_buffer = None;
    }

    fn set_format(&mut self, format: Option<FrameFormat>) {
        // A resize must never pair a frame from the old format with new dimensions.
        self.frame_buffer = None;
        self.format = format;
        self.last_capture_time = None;
    }
}

fn duplicate_fd(fd: i32) -> std::io::Result<OwnedFd> {
    // fcntl validates the integer before constructing an OwnedFd. BorrowedFd's
    // unsafe constructor would require callers to have supplied a valid fd.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: fcntl returned a new, independently owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
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
    ///     fd: Borrowed PipeWire descriptor; the stream owns a duplicate.
    ///     node_id: PipeWire node ID for the stream.
    ///     width: Portal size hint (actual dimensions are negotiated with PipeWire).
    ///     height: Portal size hint (actual dimensions are negotiated with PipeWire).
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
        if !capture_interval.is_finite() || capture_interval < 0.0 {
            return Err(PyValueError::new_err(
                "capture_interval must be finite and non-negative",
            ));
        }
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
            // Don't use portal dimensions - they may be invalid (e.g., 1x1 on Niri).
            // Wait for on_param_changed to provide actual negotiated dimensions.
            shared: Arc::new(Mutex::new(SharedState::default())),
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
        let (frame_data, format) = py.allow_threads(|| {
            let shared = self.shared.lock();
            (shared.frame_buffer.clone(), shared.format)
        });

        match (frame_data, format) {
            (Some(data), Some(format)) => {
                let expected = format
                    .frame_len()
                    .ok_or_else(|| CaptureError::PipeWire("Video frame size overflow".into()))?;
                if data.len() != expected {
                    return Err(CaptureError::FrameSizeMismatch {
                        expected,
                        actual: data.len(),
                    }
                    .into());
                }

                // Create numpy array with shape (height, width, 4)
                let array = numpy::PyArray1::from_vec_bound(py, data);
                let array = array.reshape([format.height as usize, format.width as usize, 4])?;
                Ok(Some(array))
            }
            _ => Ok(None),
        }
    }

    /// Check if capture has ended, including connection or negotiation failure.
    #[getter]
    pub fn window_invalid(&self) -> bool {
        self.shared.lock().stream_ended
    }

    /// Error from the capture thread, or None for a normal stop/window close.
    #[getter]
    pub fn error(&self) -> Option<String> {
        self.shared.lock().error.clone()
    }

    /// Stop capturing and release resources.
    pub fn stop(&mut self) -> PyResult<()> {
        // Also release resources if start() was never called.
        self.fd.take();
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
        self.shared.lock().frame_buffer = None;
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
            shared
                .lock()
                .fail(format!("Failed to create PipeWire main loop: {e}"));
            return;
        }
    };

    let context = match Context::new(&mainloop) {
        Ok(ctx) => ctx,
        Err(e) => {
            error!("Failed to create PipeWire context: {}", e);
            shared
                .lock()
                .fail(format!("Failed to create PipeWire context: {e}"));
            return;
        }
    };

    let core = match context.connect_fd(fd, None) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to connect to PipeWire via fd: {}", e);
            shared
                .lock()
                .fail(format!("Failed to connect to PipeWire: {e}"));
            return;
        }
    };

    info!(node_id, "Connected to PipeWire");

    // Core failures (for example a disconnected server) need not produce a
    // stream state change. Preserve the reason for the Python caller.
    let error_shared = Arc::clone(&shared);
    let weak_ml = mainloop.downgrade();
    let _core_listener = core
        .add_listener_local()
        .error(move |_id, _seq, _res, message| {
            error_shared.lock().fail(message.to_string());
            if let Some(ml) = weak_ml.upgrade() {
                ml.quit();
            }
        })
        .register();

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
            shared
                .lock()
                .fail(format!("Failed to set up PipeWire stream: {e}"));
            return;
        }
    };

    // Add timer to check for stop commands
    let weak_ml = mainloop.downgrade();
    let stop_shared = Arc::clone(&shared);
    let timer_callback = move |_expirations: u64| {
        if command_rx.try_recv().is_ok() || stop_shared.lock().stream_ended {
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

    let mut shared = shared.lock();
    shared.stream_ended = true;
    shared.frame_buffer = None;
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
            data.shared.lock().fail(msg.to_string());
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
                shared.frame_buffer = None;
                if let Some(ml) = data.weak_mainloop.upgrade() {
                    ml.quit();
                }
            } else {
                // Stream went to Unconnected without ever streaming - likely a negotiation failure
                warn!(
                    ?old,
                    "Stream disconnected before streaming started (possible negotiation failure)"
                );
                data.shared
                    .lock()
                    .fail("Stream disconnected before capture started".into());
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

    data.shared.lock().set_format(None);
    let Some(param) = param else {
        return;
    };

    // Parse video format using spa_sys
    let info = unsafe {
        let mut video_info: spa_sys::spa_video_info_raw = std::mem::zeroed();
        let result = spa_sys::spa_format_video_raw_parse(param.as_raw_ptr(), &mut video_info);
        if result < 0 {
            data.shared
                .lock()
                .fail("Failed to parse video format".into());
            return;
        }
        video_info
    };

    info!(
        width = info.size.width,
        height = info.size.height,
        format = info.format,
        "Video format negotiated with PipeWire"
    );

    {
        data.shared.lock().set_format(Some(FrameFormat {
            width: info.size.width,
            height: info.size.height,
            pixel_format: info.format,
        }));
    }
}

/// Process incoming video frames.
fn on_process(stream: &StreamRef, data: &mut StreamUserData) {
    // The RAII guard returns the buffer on every path, including dropped frames.
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let now = Instant::now();
    let format = {
        let shared = data.shared.lock();
        if shared.stream_ended
            || shared
                .last_capture_time
                .is_some_and(|last| now.duration_since(last).as_secs_f64() < data.capture_interval)
        {
            return;
        }
        shared.format
    };
    let Some(format) = format else {
        return;
    };
    let Some(first_data) = buffer.datas_mut().first() else {
        return;
    };
    let raw = first_data.as_raw();
    // MAP_BUFFERS maps MemFd memory but does not change its data type.
    if !matches!(
        raw.type_,
        spa_sys::SPA_DATA_MemPtr | spa_sys::SPA_DATA_MemFd
    ) {
        data.shared.lock().fail(format!(
            "Unsupported PipeWire buffer type {}: CPU-mapped MemPtr or MemFd is required",
            raw.type_
        ));
        return;
    }
    if raw.data.is_null() || raw.chunk.is_null() || raw.maxsize == 0 {
        return;
    }
    // SAFETY: PipeWire owns this mapped allocation for the lifetime of the
    // dequeued buffer. Bound the slice by maxsize, never by unvalidated chunk
    // metadata. copy_frame validates offset, size and stride before reading.
    let memory = unsafe { std::slice::from_raw_parts(raw.data.cast::<u8>(), raw.maxsize as usize) };
    let chunk = unsafe { &*raw.chunk };
    match copy_frame(memory, chunk, format) {
        Ok(Some(frame)) => {
            let mut shared = data.shared.lock();
            shared.frame_buffer = Some(frame);
            shared.last_capture_time = Some(now);
        }
        Ok(None) => {}
        Err(message) => warn!(message, "Dropping invalid video frame"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn duplicated_descriptor_does_not_close_the_callers_descriptor() {
        let original = std::fs::File::open("/dev/null").unwrap();
        let duplicate = duplicate_fd(original.as_raw_fd()).unwrap();
        assert_ne!(duplicate.as_raw_fd(), original.as_raw_fd());
        drop(duplicate);
        assert!(original.metadata().is_ok());
        assert!(duplicate_fd(-1).is_err());
    }

    #[test]
    fn renegotiation_discards_previous_frame_and_throttle() {
        let mut state = SharedState {
            frame_buffer: Some(vec![0; 8]),
            format: Some(FrameFormat {
                width: 2,
                height: 1,
                pixel_format: spa_sys::SPA_VIDEO_FORMAT_BGRA,
            }),
            last_capture_time: Some(Instant::now()),
            ..SharedState::default()
        };
        state.set_format(Some(FrameFormat {
            width: 1,
            height: 1,
            pixel_format: spa_sys::SPA_VIDEO_FORMAT_RGBA,
        }));
        assert!(state.frame_buffer.is_none());
        assert!(state.last_capture_time.is_none());
        assert_eq!(state.format.unwrap().width, 1);
    }

    #[test]
    fn capture_failure_ends_stream_without_leaving_a_stale_frame() {
        let mut state = SharedState {
            frame_buffer: Some(vec![0; 4]),
            ..SharedState::default()
        };
        state.fail("Connection refused".into());
        assert!(state.stream_ended);
        assert!(state.frame_buffer.is_none());
        assert_eq!(state.error.as_deref(), Some("Connection refused"));
    }
}
