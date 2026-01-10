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

## test_capture.py

Tests the full capture flow: portal selection + frame capture.

```bash
uv run python examples/test_capture.py
```

**Expected behavior:**
1. A system window picker dialog appears
2. Select any window
3. Script captures frames for 5 seconds
4. Prints frame info for each captured frame

**Success output:**
```
PipeWire capture available: True
Opening window picker...
Selection result: True
Stream info: (11, 84, 2560, 1440)
  File descriptor: 11
  Node ID: 84
  Window size: 2560x1440

Starting capture stream...
Capturing for 5 seconds (or until window closes)...
  Frame 1: shape=(1440, 2560, 4), dtype=uint8
  Frame 2: shape=(1440, 2560, 4), dtype=uint8
  ...

Done. Captured 50 frames in 5.0s
```

**If window closed during capture:**
```
...
Window was closed during capture.

Done. Captured 23 frames in 2.3s
```
