# Claude Context for pipewire-capture

## Project Purpose

This library solves installation failures on **Steam Deck** and **Nobara Linux** for the [interpreter](https://github.com/bquenin/interpreter) project.

**Root cause**: `PyGObject` depends on `pycairo` which has no Linux wheels on PyPI. Users must install system dev libraries (`libcairo2-dev`, `libgirepository1.0-dev`, etc.) which fails on Steam Deck (read-only filesystem) and is error-prone on Nobara.

**Solution**: Replace PyGObject/GStreamer with a pure Rust library using `pipewire-rs` + `PyO3`, distributed as pre-built manylinux wheels requiring zero system dependencies.

Related issues:
- https://github.com/bquenin/interpreter/issues/163 (Steam Deck)
- https://github.com/bquenin/interpreter/issues/180 (Nobara)

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                     Python Application                       │
│                                                              │
│  from pipewire_capture import PortalCapture, CaptureStream  │
└─────────────────────┬───────────────────────────────────────┘
                      │
┌─────────────────────▼───────────────────────────────────────┐
│                   Python API Layer                           │
│                 python/pipewire_capture/                     │
│                                                              │
│  - __init__.py: Re-exports from _native module              │
│  - py.typed: Type hints marker                              │
└─────────────────────┬───────────────────────────────────────┘
                      │
┌─────────────────────▼───────────────────────────────────────┐
│                   Rust Native Module                         │
│                      src/lib.rs                              │
│                                                              │
│  _native module exposes:                                    │
│  - is_available() -> bool                                   │
│  - PortalCapture class                                      │
│  - CaptureStream class                                      │
└──────────┬──────────────────────────────────┬───────────────┘
           │                                  │
┌──────────▼──────────┐          ┌───────────▼────────────────┐
│   src/portal.rs     │          │      src/stream.rs         │
│                     │          │                            │
│ xdg-desktop-portal  │          │  PipeWire frame capture    │
│ D-Bus integration   │          │                            │
│                     │          │  - Connect to PipeWire     │
│ - CreateSession     │ ──────▶  │  - Receive video frames    │
│ - SelectSources     │ fd,node  │  - Convert to numpy BGRA   │
│ - Start (picker)    │          │  - Detect window close     │
│ - OpenPipeWireRemote│          │                            │
└─────────────────────┘          └────────────────────────────┘
```

## Key Technical Concepts

### xdg-desktop-portal ScreenCast
D-Bus API for screen/window capture on Wayland. Flow:
1. `CreateSession` - Create a capture session
2. `SelectSources` - Configure what to capture (WINDOW type)
3. `Start` - Shows system window picker, user selects window
4. `OpenPipeWireRemote` - Get file descriptor for PipeWire stream

Portal D-Bus interface: `org.freedesktop.portal.ScreenCast`

### PipeWire
Linux multimedia server that handles video/audio streams. After portal provides `fd` and `node_id`:
1. Connect to PipeWire using the fd
2. Create a stream targeting the node_id
3. Receive video buffers via stream callbacks
4. Convert buffers to BGRA numpy arrays

### zbus
Rust async D-Bus library. Used for portal communication. Requires tokio runtime.

### pipewire-rs
Official Rust bindings for PipeWire. Handles stream setup and frame reception.

## Current Implementation Status

| Component | Status | Notes |
|-----------|--------|-------|
| Project structure | Done | Cargo.toml, pyproject.toml, CI/CD |
| `is_available()` | Partial | Checks WAYLAND_DISPLAY, TODO: check portal |
| `PortalCapture` | Skeleton | Struct defined, methods are stubs |
| `CaptureStream` | Skeleton | Struct defined, methods are stubs |
| D-Bus integration | TODO | Need zbus implementation |
| PipeWire capture | TODO | Need pipewire-rs implementation |
| Wheel building | Setup | CI configured, untested |

## GitHub Issues for Tracking

1. **Issue #1**: Implement xdg-desktop-portal ScreenCast integration
2. **Issue #2**: Implement PipeWire stream capture
3. **Issue #3**: Set up manylinux wheel building
4. **Issue #4**: Test on Steam Deck and Nobara
5. **Issue #5**: Publish v0.1.0 to PyPI

## Development Workflow

### Prerequisites (Linux only)
```bash
# Install PipeWire dev libraries
sudo apt install libpipewire-0.3-dev libspa-0.2-dev libclang-dev

# Or on Fedora/Nobara
sudo dnf install pipewire-devel libclang-devel
```

### Build and test locally
```bash
# Install maturin
pip install maturin

# Development build (creates .so in-place)
maturin develop

# Test in Python
python -c "from pipewire_capture import is_available; print(is_available())"

# Release build
maturin build --release
```

### Run Rust checks
```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo build --release
```

## Code Style

- **Rust**: Follow standard Rust conventions, use `cargo fmt`
- **Python**: Minimal Python code, mostly re-exports from native module
- **Error handling**: Use `thiserror` for Rust errors, convert to `PyErr` at boundary
- **Threading**: PipeWire runs its own main loop; use `parking_lot` for synchronization

## Integration with interpreter

After this library is published, the interpreter project needs updates:

1. **pyproject.toml**: Replace `PyGObject>=3.42.0` with `pipewire-capture>=0.1.0`
2. **linux_wayland.py**: Rewrite to use new API (see integration plan in interpreter repo)

Integration plan saved at: `bquenin/interpreter` branch `feature/pipewire-capture-integration`
File: `docs/pipewire-capture-integration.md`

## Testing Strategy

1. **Unit tests**: Test Rust components individually
2. **Integration test**: Full flow on Wayland session
3. **Platform tests**: Steam Deck (SteamOS), Nobara, Fedora, Ubuntu
4. **CI**: Build wheels in GitHub Actions (can't test capture without display)

## Common Issues

### "PipeWire not available"
- Ensure running on Wayland (`echo $WAYLAND_DISPLAY` should show value)
- Ensure PipeWire is running (`systemctl --user status pipewire`)

### Build fails with "pipewire.h not found"
- Install PipeWire dev libraries (see Prerequisites above)

### D-Bus permission denied
- Portal requires a desktop session
- Won't work in pure SSH/container without D-Bus session bus
