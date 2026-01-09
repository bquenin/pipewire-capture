# Examples

Manual test scripts for pipewire-capture. These require a Wayland session.

## Prerequisites

```bash
# Install maturin (build tool)
uv pip install maturin

# Build the native extension
uv run maturin develop
```

## test_portal.py

Tests the xdg-desktop-portal window selection flow.

```bash
uv run python examples/test_portal.py
```

**Expected behavior:**
1. A system window picker dialog appears
2. Select any window
3. Script prints the PipeWire stream info (fd, node_id, width, height)

**Success output:**
```
PipeWire capture available: True
Opening window picker...
Selection result: True
Stream info: (11, 84, 2560, 1440)
  File descriptor: 11
  Node ID: 84
  Window size: 2560x1440

Success! Portal integration is working.
```

**If cancelled:**
```
PipeWire capture available: True
Opening window picker...
Selection result: False
Stream info: None

No stream info - selection was cancelled or failed.
```
