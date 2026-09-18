# pipewire-capture

[![PyPI version](https://badge.fury.io/py/pipewire-capture.svg)](https://pypi.org/project/pipewire-capture/)

Python library for PipeWire video capture with pre-built wheels.

This library provides PipeWire-based video capture for Wayland Linux environments, using the xdg-desktop-portal ScreenCast interface for window selection. Pre-built wheels avoid compilation; the PipeWire runtime and a compatible desktop portal must be installed on the system.

## Features

- **Window selection** via xdg-desktop-portal (works on GNOME, KDE, Gamescope, etc.)
- **Frame capture** via PipeWire streams
- **Pre-built wheels** - no compilation required
- **Python 3.9+** support via stable ABI

## Installation

```bash
uv pip install pipewire-capture
```

## Requirements

- Linux with Wayland
- PipeWire (installed by default on modern Linux distributions)
- xdg-desktop-portal with ScreenCast support

## Usage

```python
import time
from pipewire_capture import PortalCapture, CaptureStream, is_available

# This checks for the portal interface; capture can still fail later.
if not is_available():
    print("ScreenCast portal is unavailable")
    exit(1)

session = PortalCapture().select_window()  # Blocking system window picker
if session is None:
    print("Window selection cancelled")
else:
    try:
        stream = CaptureStream(session.fd, session.node_id, session.width, session.height)
        try:
            stream.start()
            for _ in range(10):
                if stream.window_invalid:
                    if stream.error:
                        raise RuntimeError(stream.error)
                    break
                frame = stream.get_frame()  # numpy array (H, W, 4), BGRA
                if frame is not None:
                    print(f"Got frame: {frame.shape}")
                time.sleep(0.25)
        finally:
            stream.stop()
    finally:
        session.close()
```

## API Reference

### `is_available() -> bool`

Check whether the ScreenCast portal interface exists on the session bus. The result is cached for the process lifetime. This does not guarantee that permission will be granted or that the compositor can create a stream.

### `PortalCapture`

Handles window selection via xdg-desktop-portal.

- `select_window()` - Show the window picker synchronously. Returns a `PortalSession`, or `None` if cancelled. Raises `RuntimeError` on failure, including the failing portal step.

### `PortalSession`

Keep the session alive for the duration of capture.

- `fd`, `node_id`, `width`, `height` - Stream information. Width and height are portal hints; use the captured array's shape for actual dimensions.
- `fd` is borrowed from the session. Do not close it yourself. `CaptureStream` duplicates it and closes its own descriptor independently.
- When supplying a descriptor from another source, the caller remains responsible for closing that original descriptor. This replaces the previous implicit ownership transfer; the usual `CaptureStream(session.fd, ...)` call stays the same.
- `is_open` - Whether the session has been closed locally.
- `close()` - Close the descriptor and request portal cleanup. Safe to call more than once. `fd` becomes `-1`. Dropping the session also closes the descriptor and schedules portal cleanup.

### `CaptureStream`

Captures frames from a PipeWire stream.

- `CaptureStream(fd, node_id, width, height, capture_interval=0.25)` - Create stream
- `start()` - Start the capture thread. A stopped stream cannot be restarted; create a new session and stream.
- `get_frame()` - Get latest frame as numpy array (BGRA)
- `window_invalid` - True when capture ends, including window closure or a connection/negotiation error.
- `error` - Background capture failure message, or `None` for normal closure. Check this when `window_invalid` becomes true.
- `stop()` - Stop capturing and release resources, including when `start()` was never called.

`capture_interval` must be finite and non-negative; zero disables throttling. Packed BGRA, BGRx, RGBA and RGBx frames in CPU-mapped MemPtr/MemFd buffers are normalized to tightly packed BGRA. Row padding is removed and unused alpha bytes become opaque. DMA-BUF-only streams and negative row strides are not supported. Negotiated dimensions are currently limited to 4096 by 4096.

### KDE capture failures

If portal `Start` fails and KWin confirms a `qpainter` or `none` compositing backend, the library raises `UnsupportedCompositorError`, a subclass of `RuntimeError`. It includes recovery guidance and the original portal error. Missing or unresponsive KWin diagnostics preserve the original failure.

KWin requires OpenGL compositing for screencasting. Update KDE and your graphics drivers, then log out and back in. If the failure persists, inspect `qdbus6 org.kde.KWin /KWin org.kde.KWin.supportInformation` and the KWin logs. An X11 desktop session is a possible workaround. Changing PipeWire buffer formats cannot make KWin create a stream when its compositor rejects the request.

Applications may catch `UnsupportedCompositorError` to offer targeted help. Cancellation still returns `None`.

## Building from source

Requirements:
- Rust toolchain
- uv (installs maturin and the development dependencies)
- PipeWire development libraries

```bash
# Development build
uv sync --python 3.12
uv run maturin develop

# Release build
uv run maturin build --release

# Checks (Python tests use a private dbus-daemon, without a desktop)
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
uv run python -m unittest discover -s tests -v
```

## License

MIT
