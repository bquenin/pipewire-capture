//! Simple test binary for PipeWire capture without Python.
//!
//! This tests the basic capture flow: portal selection + PipeWire streaming.

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pipewire::spa::pod::{serialize::PodSerializer, Value};

#[tokio::main]
async fn main() {
    eprintln!("[test] Starting PipeWire capture test");

    // Check Wayland
    if std::env::var("WAYLAND_DISPLAY").is_err() {
        eprintln!("[test] ERROR: Not running on Wayland");
        return;
    }
    eprintln!("[test] Running on Wayland");

    // Portal flow
    eprintln!("[test] Opening portal...");
    let (fd, node_id, width, height) = match open_portal().await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("[test] Portal error: {}", e);
            return;
        }
    };
    eprintln!(
        "[test] Got stream: fd={}, node_id={}, {}x{}",
        fd.as_raw_fd(),
        node_id,
        width,
        height
    );

    // PipeWire streaming
    eprintln!("[test] Initializing PipeWire...");
    pipewire::init();

    let mainloop = pipewire::main_loop::MainLoop::new(None).expect("Failed to create main loop");
    let context = pipewire::context::Context::new(&mainloop).expect("Failed to create context");
    let core = context
        .connect_fd(fd, None)
        .expect("Failed to connect to PipeWire");

    eprintln!("[test] Connected to PipeWire, node_id={}", node_id);

    // Create stream
    let props = pipewire::properties::properties! {
        *pipewire::keys::MEDIA_TYPE => "Video",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Screen",
    };

    let stream = pipewire::stream::Stream::new(&core, "test-capture", props)
        .expect("Failed to create stream");

    // Track frames
    let frame_count = Arc::new(AtomicU32::new(0));
    let frame_count_clone = Arc::clone(&frame_count);

    struct TestUserData;

    let _listener = stream
        .add_local_listener_with_user_data(TestUserData)
        .state_changed(move |_stream, _data, old, new| {
            eprintln!("[test] State changed: {:?} -> {:?}", old, new);
            if matches!(new, pipewire::stream::StreamState::Streaming) {
                eprintln!("[test] Now streaming!");
            }
        })
        .param_changed(|_stream, _data, id, param| {
            if id == 4 && param.is_some() {
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
            }
        })
        .process(move |stream, _data| {
            let pw_buffer = unsafe { stream.dequeue_raw_buffer() };
            if pw_buffer.is_null() {
                return;
            }

            let spa_buffer = unsafe { (*pw_buffer).buffer };
            if !spa_buffer.is_null() {
                let n_datas = unsafe { (*spa_buffer).n_datas };
                let datas_ptr = unsafe { (*spa_buffer).datas };

                if n_datas > 0 && !datas_ptr.is_null() {
                    let first_data = unsafe { &*datas_ptr };
                    let count = frame_count_clone.fetch_add(1, Ordering::SeqCst) + 1;

                    // Log first few frames
                    if count <= 5 {
                        eprintln!(
                            "[test] Frame {}: type={}, n_datas={}",
                            count, first_data.type_, n_datas
                        );
                    }
                }
            }

            unsafe { stream.queue_raw_buffer(pw_buffer) };
        })
        .register()
        .expect("Failed to register listener");

    // Build format params with framerate
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

    // Connect stream with format params and MAP_BUFFERS
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
}

async fn open_portal() -> Result<(OwnedFd, u32, i32, i32), String> {
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
    use ashpd::desktop::PersistMode;

    eprintln!("[test] Creating session...");
    let screencast = Screencast::new().await.map_err(|e| e.to_string())?;
    let session = screencast
        .create_session()
        .await
        .map_err(|e| e.to_string())?;

    eprintln!("[test] Selecting sources (window)...");
    screencast
        .select_sources(
            &session,
            CursorMode::Hidden,
            SourceType::Window.into(),
            false,
            None,
            PersistMode::DoNot,
        )
        .await
        .map_err(|e| e.to_string())?;

    eprintln!("[test] Starting (opens window picker)...");
    let streams = screencast
        .start(&session, None)
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;

    let stream = streams.streams().first().ok_or("No stream selected")?;

    let node_id = stream.pipe_wire_node_id();
    let (width, height) = stream.size().unwrap_or((0, 0));
    eprintln!(
        "[test] Got stream: node_id={}, {}x{}",
        node_id, width, height
    );

    let fd = screencast
        .open_pipe_wire_remote(&session)
        .await
        .map_err(|e| e.to_string())?;

    Ok((fd, node_id, width, height))
}
