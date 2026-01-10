# Claude Context for pipewire-capture

## Project Overview

PipeWire-based video capture library for Python on Wayland/Linux. Uses `pipewire-rs` + `PyO3`, distributed as pre-built manylinux wheels. Requires PipeWire runtime (installed by default on modern Linux desktops).

PyPI: https://pypi.org/project/pipewire-capture/

## Development

**IMPORTANT: Always use `uv` for Python package management.**

### Prerequisites
```bash
# Ubuntu/Debian
sudo apt install libpipewire-0.3-dev libspa-0.2-dev libclang-dev

# Fedora/Nobara
sudo dnf install pipewire-devel libclang-devel
```

### Build and test
```bash
uv run maturin develop              # Build and install
uv run python examples/test_portal.py  # Test (requires Wayland)
```

### Rust checks
```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

### Pull Request workflow

**NEVER commit directly to `main`. All changes go through PRs.**

```bash
git checkout -b issue-N-description
# make changes
git push -u origin issue-N-description
gh pr create
gh pr merge --squash  # after CI passes
```

## Code Style

- **Rust**: `cargo fmt`, clippy with `-D warnings`
- **Error handling**: `thiserror` for Rust errors, convert to `PyErr` at boundary
- **Logging**: `tracing` crate (`debug!`, `info!`, `error!`)
- **Threading**: PipeWire has its own main loop; use `parking_lot` for sync
- **Version**: Single source of truth in `Cargo.toml` (Python reads via `importlib.metadata`)

## Wheel Building

- manylinux_2_34 wheels for x86_64 and aarch64
- libpipewire is **not bundled** - uses system library at runtime
- Build uses `auditwheel repair --exclude libpipewire-0.3.so.0`

## Common Issues

### "PipeWire not available"
- Must run on Wayland (`echo $WAYLAND_DISPLAY` should show value)
- PipeWire must be running (`systemctl --user status pipewire`)

### Build fails with "pipewire.h not found"
- Install PipeWire dev libraries (see Prerequisites)

### Portal hangs or permission denied
- Requires a desktop session with D-Bus
- Won't work in SSH/container without D-Bus session bus
