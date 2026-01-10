# Claude Context for pipewire-capture

## Project Purpose

This library solves installation failures on **Steam Deck** and **Nobara Linux** for the [interpreter](https://github.com/bquenin/interpreter) project.

**Root cause**: `PyGObject` depends on `pycairo` which has no Linux wheels on PyPI. Users must install system dev libraries (`libcairo2-dev`, `libgirepository1.0-dev`, etc.) which fails on Steam Deck (read-only filesystem) and is error-prone on Nobara.

**Solution**: Replace PyGObject/GStreamer with a pure Rust library using `pipewire-rs` + `PyO3`, distributed as pre-built manylinux wheels. Requires PipeWire runtime library (installed by default on modern Linux desktops).

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

### ASHPD
High-level Rust wrapper for xdg-desktop-portal (https://github.com/bilelmoussaoui/ashpd). Provides idiomatic async API for portal communication including ScreenCast. Uses zbus internally.

### pipewire-rs
Official Rust bindings for PipeWire. Handles stream setup and frame reception.

## API Design Decisions

**Pull model for CaptureStream:** We use polling (`get_frame()`) rather than callbacks. This matches the [scrap](https://lib.rs/crates/scrap) library pattern and is simpler for OCR/translation use cases where you only need the latest frame, not every frame.

**Direct return for PortalCapture:** `select_window()` returns results directly rather than via callback. One-shot blocking operations should return values; callbacks are for streams (like `on_frame_arrived` in windows-capture).

**Comparison with other libraries:**
- Push (callback): PipeWire native, OBS, windows-capture, screencapturekit-rs
- Pull (polling): scrap, our library

Both patterns are valid. Pull is appropriate here because missing frames between polls doesn't matter for the interpreter use case.

## Current Implementation Status

| Component | Status | Notes |
|-----------|--------|-------|
| Project structure | Done | Cargo.toml, pyproject.toml, CI/CD |
| Rust tooling | Done | rust-toolchain.toml (1.85.0), .rustfmt.toml, clippy |
| CI/CD | Done | GitHub Actions: lint, build, test-python, publish jobs |
| Unit tests | Done | Test infrastructure in place, basic tests for error/lib |
| Logging | Done | Using `tracing` crate for structured logging |
| `is_available()` | Done | Checks WAYLAND_DISPLAY |
| `PortalCapture` | Done | Full ASHPD integration, window picker works |
| `CaptureStream` | Done | Full PipeWire capture, ~60 fps, BGRA numpy arrays |
| D-Bus integration | Done | Using ASHPD library (PR #8) |
| PipeWire capture | Done | pipewire-rs + EGL/OpenGL for DmaBuf import |
| Wheel building | Done | manylinux_2_34 wheels for x86_64 and aarch64, excludes libpipewire |
| PyPI publishing | Done | v0.2.2 published via trusted publishing |
| Cargo.lock | Committed | Pinned deps for reproducible builds |
| Version management | Done | Single source of truth in Cargo.toml, Python reads via importlib.metadata |

## GitHub Issues (Closed)

1. **Issue #1**: ~~Implement xdg-desktop-portal ScreenCast integration~~ ✅ (PR #8)
2. **Issue #2**: ~~Implement PipeWire stream capture~~ ✅ (PR #11)
3. **Issue #3**: ~~Set up manylinux wheel building~~ ✅ (PR #12)
4. **Issue #4**: ~~Test on Steam Deck and Nobara~~ ✅ (validated on Ubuntu/Wayland)
5. **Issue #5**: ~~Publish v0.1.0 to PyPI~~ ✅ (https://pypi.org/project/pipewire-capture/)
6. **Issue #9**: ~~Add tracing for structured logging~~ ✅ (PR #10)
7. **Issue #14**: ~~callback borrow error~~ ✅ (PR #15)
8. **Issue #17**: ~~SPA plugin path error from bundled libpipewire~~ ✅ (PR #18)
9. **Issue #20**: ~~v0.2.1 still bundles libpipewire~~ ✅ (PR #21)

## Development Workflow

### Prerequisites (Linux only)
```bash
# Install PipeWire dev libraries
sudo apt install libpipewire-0.3-dev libspa-0.2-dev libclang-dev

# Or on Fedora/Nobara
sudo dnf install pipewire-devel libclang-devel
```

### Build and test locally

**IMPORTANT: Always use `uv` for Python package management in this project.**

```bash
# Build and install in development mode
uv run maturin develop

# Quick test
uv run python -c "from pipewire_capture import is_available; print(is_available())"

# Release build
uv run maturin build --release
```

### Manual testing
```bash
# Test portal window picker (requires Wayland session)
uv run python examples/test_portal.py
```

### Run Rust checks
```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

### Contributing via Pull Requests

**IMPORTANT: NEVER commit directly to `main`. All changes MUST go through pull requests.**

Workflow:
1. Create a feature branch: `git checkout -b issue-N-description`
2. Make changes and ensure all checks pass
3. Commit with descriptive message
4. Push and create PR: `gh pr create`
5. Merge after review: `gh pr merge --squash`

**Repo settings:**
- Only squash merges allowed
- Head branches auto-delete after merge

## Code Style

- **Rust**: Follow standard Rust conventions, use `cargo fmt`
- **Python**: Minimal Python code, mostly re-exports from native module
- **Error handling**: Use `thiserror` for Rust errors, convert to `PyErr` at boundary
- **Logging**: Use `tracing` crate (`debug!`, `info!`, `error!`) for structured logging
- **Threading**: PipeWire runs its own main loop; use `parking_lot` for synchronization

## Integration with interpreter

After this library is published, the interpreter project needs updates:

1. **pyproject.toml**: Replace `PyGObject>=3.42.0` with `pipewire-capture>=0.2.0`
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
