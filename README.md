# pipewire-capture

Python library for PipeWire video capture with pre-built wheels.

This library provides PipeWire-based video capture for Wayland Linux environments, using the xdg-desktop-portal ScreenCast interface for window selection. It's distributed as pre-built wheels, so no compilation or system dependencies are needed for installation.

## Features

- **Window selection** via xdg-desktop-portal (works on GNOME, KDE, Gamescope, etc.)
- **Frame capture** via PipeWire streams
- **Pre-built wheels** - no compilation required
- **Python 3.9+** support via stable ABI

## Installation

```bash
pip install pipewire-capture
```

## Requirements

- Linux with Wayland
- PipeWire (installed by default on modern Linux distributions)
- xdg-desktop-portal with ScreenCast support

## Usage

```python
from pipewire_capture import PortalCapture, CaptureStream, is_available

# Check if capture is available
if not is_available():
    print("PipeWire capture not available (not running on Wayland)")
    exit(1)

# Show window picker and select a window
portal = PortalCapture()

def on_window_selected(success: bool):
    if not success:
        print("Window selection cancelled")
        return

    # Get PipeWire stream info
    stream_info = portal.get_stream_info()
    if stream_info is None:
        print("No stream available")
        return

    fd, node_id, width, height = stream_info

    # Start capturing frames
    stream = CaptureStream(fd, node_id, width, height, capture_interval=0.25)
    stream.start()

    # Get frames
    for _ in range(10):
        frame = stream.get_frame()  # numpy array (H, W, 4) BGRA
        if frame is not None:
            print(f"Got frame: {frame.shape}")

        if stream.window_invalid:
            print("Window was closed")
            break

    stream.stop()
    portal.close()

portal.select_window(on_window_selected)
```

## API Reference

### `is_available() -> bool`

Check if PipeWire capture is available on this system.

### `PortalCapture`

Handles window selection via xdg-desktop-portal.

- `select_window(callback)` - Show window picker, calls callback with success/failure
- `get_stream_info()` - Returns `(fd, node_id, width, height)` tuple or `None`
- `close()` - Release resources

### `CaptureStream`

Captures frames from a PipeWire stream.

- `CaptureStream(fd, node_id, width, height, capture_interval=0.25)` - Create stream
- `start()` - Start capturing
- `get_frame()` - Get latest frame as numpy array (BGRA)
- `window_invalid` - Property: True if window was closed
- `stop()` - Stop capturing

## Building from source

Requirements:
- Rust toolchain
- maturin (`pip install maturin`)
- PipeWire development libraries

```bash
# Development build
maturin develop

# Release build
maturin build --release
```

## License

MIT
