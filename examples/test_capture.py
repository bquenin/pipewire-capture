#!/usr/bin/env python3
"""Test the full capture flow: portal selection + frame capture."""

import time
from pipewire_capture import PortalCapture, CaptureStream, is_available

print(f"PipeWire capture available: {is_available()}")

if not is_available():
    print("Not running on Wayland, exiting")
    exit(1)

# Step 1: Select a window via portal
print("Opening window picker...")
portal = PortalCapture()
info = portal.select_window()

print(f"Stream info: {info}")

if not info:
    print("\nNo stream info - selection was cancelled.")
    exit(1)

fd, node_id, width, height = info
print(f"  File descriptor: {fd}")
print(f"  Node ID: {node_id}")
print(f"  Window size: {width}x{height}")

# Step 2: Create and start capture stream
print("\nStarting capture stream...")
stream = CaptureStream(fd, node_id, width, height, capture_interval=0.1)
stream.start()

# Step 3: Capture frames for 5 seconds
print("Capturing for 5 seconds (or until window closes)...")
start_time = time.time()
frame_count = 0

while time.time() - start_time < 5 and not stream.window_invalid:
    frame = stream.get_frame()
    if frame is not None:
        frame_count += 1
        print(f"  Frame {frame_count}: shape={frame.shape}, dtype={frame.dtype}")
    time.sleep(0.1)

# Step 4: Stop and cleanup
stream.stop()

if stream.window_invalid:
    print("\nWindow was closed during capture.")

print(f"\nDone. Captured {frame_count} frames in {time.time() - start_time:.1f}s")
