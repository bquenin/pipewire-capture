//! Simple test binary for PipeWire capture without Python.

use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pipewire::spa::pod::builder::{builder_add, Builder};
use pipewire::spa::sys as spa_sys;
use pipewire::spa::utils::Id;
use pipewire::stream::StreamRef;

// SPA constants for explicit sync
const SPA_TYPE_OBJECT_PARAM_META: u32 = spa_sys::SPA_TYPE_OBJECT_ParamMeta;
const SPA_PARAM_META: u32 = spa_sys::SPA_PARAM_Meta;
const SPA_PARAM_META_TYPE: u32 = spa_sys::SPA_PARAM_META_type;
const SPA_PARAM_META_SIZE: u32 = spa_sys::SPA_PARAM_META_size;
const SPA_META_SYNC_TIMELINE: u32 = spa_sys::SPA_META_SyncTimeline;

use std::fs::OpenOptions;
use std::os::fd::BorrowedFd;
use std::sync::OnceLock;

static DRM_FD: OnceLock<OwnedFd> = OnceLock::new();

fn get_drm_fd() -> Option<BorrowedFd<'static>> {
    let fd = DRM_FD.get_or_init(|| {
        for path in &["/dev/dri/renderD128", "/dev/dri/renderD129"] {
            if let Ok(file) = OpenOptions::new().read(true).write(true).open(path) {
                use std::os::fd::IntoRawFd;
                let raw_fd = file.into_raw_fd();
                eprintln!("[test] Opened DRM render node: {}", path);
                return unsafe { OwnedFd::from_raw_fd(raw_fd) };
            }
        }
        eprintln!("[test] Failed to open any DRM render node");
        unsafe { OwnedFd::from_raw_fd(-1) }
    });

    if fd.as_raw_fd() < 0 {
        None
    } else {
        Some(unsafe { BorrowedFd::borrow_raw(fd.as_raw_fd()) })
    }
}

/// Test waiting on a syncobj with timeline_wait
fn test_syncobj_wait(syncobj_fd: i32, acquire_point: u64) {
    let Some(drm_fd) = get_drm_fd() else {
        eprintln!("[test] No DRM device for syncobj wait");
        return;
    };

    // Import syncobj fd as handle
    let syncobj = unsafe { BorrowedFd::borrow_raw(syncobj_fd) };
    let handle = match drm_ffi::syncobj::fd_to_handle(drm_fd, syncobj, false) {
        Ok(r) => r.handle,
        Err(e) => {
            eprintln!("[test] Failed to import syncobj: {}", e);
            return;
        }
    };

    // Get current time and set timeout
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    let now_nsec = ts.tv_sec * 1_000_000_000 + ts.tv_nsec;
    let timeout_nsec = now_nsec + 1_000_000_000; // 1 second

    let start = Instant::now();
    match drm_ffi::syncobj::timeline_wait(
        drm_fd,
        &[handle],
        &[acquire_point],
        timeout_nsec,
        true,  // wait_all
        true,  // wait_for_submit
        false, // wait_available
    ) {
        Ok(_) => eprintln!("[test]   Syncobj wait succeeded in {:?}", start.elapsed()),
        Err(e) => eprintln!(
            "[test]   Syncobj wait FAILED: {} (after {:?})",
            e,
            start.elapsed()
        ),
    }

    let _ = drm_ffi::syncobj::destroy(drm_fd, handle);
}

/// Request SyncTimeline metadata from PipeWire for explicit synchronization.
fn request_sync_timeline_metadata(stream: &StreamRef) {
    let mut pod_data = Vec::with_capacity(256);
    let mut builder = Builder::new(&mut pod_data);

    let result = builder_add!(
        &mut builder,
        Object(SPA_TYPE_OBJECT_PARAM_META, SPA_PARAM_META) {
            SPA_PARAM_META_TYPE => Id(Id(SPA_META_SYNC_TIMELINE)),
            SPA_PARAM_META_SIZE => Int(size_of::<spa_sys::spa_meta_sync_timeline>() as i32),
        }
    );

    if let Err(e) = result {
        eprintln!("[test] Failed to build SyncTimeline meta param: {:?}", e);
        return;
    }

    let pod = unsafe {
        let raw_pod = spa_sys::spa_pod_builder_deref(builder.as_raw_ptr(), 0);
        if raw_pod.is_null() {
            eprintln!("[test] Failed to get pod from builder");
            return;
        }
        &*(raw_pod as *const pipewire::spa::pod::Pod)
    };

    let mut params = [pod];
    if let Err(e) = stream.update_params(&mut params) {
        eprintln!("[test] Failed to update params with SyncTimeline: {:?}", e);
    } else {
        eprintln!("[test] Requested SyncTimeline metadata for explicit sync");
    }
}

fn main() {
    eprintln!("[test] Starting PipeWire capture test");

    // Check if running on Wayland
    if std::env::var("WAYLAND_DISPLAY").is_err() {
        eprintln!("[test] Not running on Wayland, exiting");
        std::process::exit(1);
    }

    eprintln!("[test] Running on Wayland");

    // Use tokio runtime for portal
    let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");

    // Run the portal flow
    let stream_info = runtime.block_on(async {
        use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
        use ashpd::desktop::PersistMode;

        eprintln!("[test] Opening portal...");
        let proxy = Screencast::new().await?;

        eprintln!("[test] Creating session...");
        let session = proxy.create_session().await?;

        eprintln!("[test] Selecting sources (window)...");
        proxy
            .select_sources(
                &session,
                CursorMode::Hidden,
                SourceType::Window.into(),
                false,
                None,
                PersistMode::DoNot,
            )
            .await?;

        eprintln!("[test] Starting (opens window picker)...");
        let response = proxy.start(&session, None).await?;

        let streams = response.response()?;
        if streams.streams().is_empty() {
            eprintln!("[test] No streams returned");
            return Err(ashpd::Error::NoResponse);
        }

        let stream = &streams.streams()[0];
        let node_id = stream.pipe_wire_node_id();
        let (width, height) = stream.size().unwrap_or((1920, 1080));

        eprintln!(
            "[test] Got stream: node_id={}, {}x{}",
            node_id, width, height
        );

        // Get PipeWire remote fd
        let fd = proxy.open_pipe_wire_remote(&session).await?;
        let raw_fd = fd.as_raw_fd();

        // Duplicate the fd so we own it
        let dup_fd = unsafe { libc::dup(raw_fd) };
        if dup_fd < 0 {
            eprintln!("[test] Failed to dup fd");
            return Err(ashpd::Error::NoResponse);
        }

        Ok::<_, ashpd::Error>((dup_fd, node_id, width, height))
    });

    let (fd, node_id, width, height) = match stream_info {
        Ok(info) => info,
        Err(e) => {
            eprintln!("[test] Portal error: {}", e);
            std::process::exit(1);
        }
    };

    eprintln!(
        "[test] Got stream: fd={}, node_id={}, {}x{}",
        fd, node_id, width, height
    );

    // Now test the PipeWire stream directly
    eprintln!("[test] Initializing PipeWire...");
    pipewire::init();

    let mainloop = pipewire::main_loop::MainLoop::new(None).expect("Failed to create main loop");
    let context = pipewire::context::Context::new(&mainloop).expect("Failed to create context");

    let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let core = context
        .connect_fd(owned_fd, None)
        .expect("Failed to connect to PipeWire");

    eprintln!("[test] Connected to PipeWire, node_id={}", node_id);

    // Create stream
    use pipewire::properties::properties;
    let props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Video",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Screen",
    };

    let stream = pipewire::stream::Stream::new(&core, "test-capture", props)
        .expect("Failed to create stream");

    // Track frames
    let frame_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let frame_count_clone = Arc::clone(&frame_count);

    // User data for stream callbacks (not used in this test)
    struct TestUserData;

    let listener = stream
        .add_local_listener_with_user_data(TestUserData)
        .state_changed(move |_stream, _data, old, new| {
            eprintln!("[test] State changed: {:?} -> {:?}", old, new);
            if matches!(new, pipewire::stream::StreamState::Streaming) {
                eprintln!("[test] Now streaming!");
            }
        })
        .param_changed(|stream, _data, id, param| {
            if id == 4 && param.is_some() {
                eprintln!("[test] Format param changed, id={}", id);
                // Parse and log format
                if let Some(p) = param {
                    let mut video_info: pipewire::spa::sys::spa_video_info_raw =
                        unsafe { std::mem::zeroed() };
                    let result = unsafe {
                        pipewire::spa::sys::spa_format_video_raw_parse(
                            p.as_raw_ptr(),
                            &mut video_info,
                        )
                    };
                    if result >= 0 {
                        eprintln!(
                            "[test] Video format: {}x{}, format={}",
                            video_info.size.width, video_info.size.height, video_info.format
                        );
                    }
                }

                // Request SyncTimeline metadata
                request_sync_timeline_metadata(stream);
            }
        })
        .process(move |stream, _data| {
            let pw_buffer = unsafe { stream.dequeue_raw_buffer() };
            if pw_buffer.is_null() {
                return;
            }

            let spa_buffer = unsafe { (*pw_buffer).buffer };
            if spa_buffer.is_null() {
                unsafe { stream.queue_raw_buffer(pw_buffer) };
                return;
            }

            let n_datas = unsafe { (*spa_buffer).n_datas };
            let datas_ptr = unsafe { (*spa_buffer).datas };

            if n_datas == 0 || datas_ptr.is_null() {
                unsafe { stream.queue_raw_buffer(pw_buffer) };
                return;
            }

            let first_data = unsafe { &*datas_ptr };
            let count = frame_count_clone.fetch_add(1, Ordering::SeqCst) + 1;

            // Log first few frames
            if count <= 5 {
                eprintln!(
                    "[test] Frame {}: type={}, fd={}, n_datas={}",
                    count, first_data.type_, first_data.fd, n_datas
                );

                // Check for SyncTimeline metadata
                use std::mem::size_of;
                let stl = unsafe {
                    pipewire::spa::sys::spa_buffer_find_meta_data(
                        spa_buffer,
                        pipewire::spa::sys::SPA_META_SyncTimeline,
                        size_of::<pipewire::spa::sys::spa_meta_sync_timeline>(),
                    ) as *const pipewire::spa::sys::spa_meta_sync_timeline
                };

                if !stl.is_null() {
                    let acquire_point = unsafe { (*stl).acquire_point };
                    eprintln!("[test]   SyncTimeline: acquire_point={}", acquire_point);

                    // Log data types
                    for i in 0..n_datas {
                        let data = unsafe { &*datas_ptr.add(i as usize) };
                        eprintln!("[test]   datas[{}]: type={}, fd={}", i, data.type_, data.fd);
                    }

                    // Try waiting on the syncobj
                    if n_datas >= 2 && acquire_point > 0 {
                        let acquire_data = unsafe { &*datas_ptr.add(1) };
                        let syncobj_fd = acquire_data.fd as i32;
                        if syncobj_fd >= 0 {
                            test_syncobj_wait(syncobj_fd, acquire_point);
                        }
                    }
                } else {
                    eprintln!("[test]   No SyncTimeline metadata");
                }
            }

            // Just count frames, don't stop early - let timer handle it

            unsafe { stream.queue_raw_buffer(pw_buffer) };
        })
        .register()
        .expect("Failed to register listener");

    // Build format params with framerate
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
            .unwrap()
            .0
            .into_inner();

    let mut params = [pipewire::spa::pod::Pod::from_bytes(&values).unwrap()];

    // Connect stream with format params
    stream
        .connect(
            pipewire::spa::utils::Direction::Input,
            Some(node_id),
            pipewire::stream::StreamFlags::AUTOCONNECT | pipewire::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .expect("Failed to connect stream");

    eprintln!("[test] Stream connected, running main loop...");

    // Run for exactly 5 seconds to measure FPS
    let start = Instant::now();
    let timer_callback = {
        let weak_ml = mainloop.downgrade();
        move |_: u64| {
            if start.elapsed() > Duration::from_secs(5) {
                if let Some(ml) = weak_ml.upgrade() {
                    ml.quit();
                }
            }
        }
    };

    let timer = mainloop.loop_().add_timer(timer_callback);
    timer.update_timer(
        Some(Duration::from_millis(100)),
        Some(Duration::from_millis(100)),
    );

    mainloop.run();

    let total_frames = frame_count.load(Ordering::SeqCst);
    let elapsed = start.elapsed();
    eprintln!(
        "[test] Done. Captured {} frames in {:.1}s ({:.1} fps)",
        total_frames,
        elapsed.as_secs_f64(),
        total_frames as f64 / elapsed.as_secs_f64()
    );

    drop(listener);
}
