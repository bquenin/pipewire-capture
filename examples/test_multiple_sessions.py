#!/usr/bin/env python3
"""Test multiple capture sessions (reproduces issue #26).

This test verifies that after stopping a capture and starting a new one,
the second CaptureStream receives frames correctly.
"""

import time
from pipewire_capture import PortalCapture, CaptureStream, is_available

print(f"PipeWire capture available: {is_available()}")

if not is_available():
    print("Not running on Wayland, exiting")
    exit(1)

portal = PortalCapture()


def do_capture(label: str) -> bool:
    """Run a capture session and return True if frames were received."""
    print(f"\n{'='*50}")
    print(f"{label}: Opening window picker...")

    session = portal.select_window()
    if not session:
        print(f"{label}: Selection cancelled")
        return False

    print(f"{label}: Session: {session}")

    stream = CaptureStream(session.fd, session.node_id, session.width, session.height, 0.1)
    stream.start()

    # Capture for 5 seconds, counting frames
    print(f"{label}: Capturing for 5 seconds...")
    frame_count = 0
    start_time = time.time()
    while time.time() - start_time < 5.0:
        frame = stream.get_frame()
        if frame is not None:
            frame_count += 1
        time.sleep(0.1)

    got_frame = frame_count > 0
    print(f"{label}: Captured {frame_count} frames in 5 seconds")
    print(f"{label}: window_invalid={stream.window_invalid}, session.is_open={session.is_open}")

    print(f"{label}: Stopping stream...")
    stream.stop()
    print(f"{label}: Stream stopped")

    print(f"{label}: Closing session...")
    session.close()
    print(f"{label}: Session closed, is_open={session.is_open}")

    return got_frame


# Run two captures
first_ok = do_capture("First capture")

print("\n" + "="*50)
print("Waiting 2 seconds before second capture...")
print("="*50)
time.sleep(2)

second_ok = do_capture("Second capture")

print(f"\n{'='*50}")
print("Results:")
print(f"  First capture:  {'PASS' if first_ok else 'FAIL'}")
print(f"  Second capture: {'PASS' if second_ok else 'FAIL'}")

if first_ok and second_ok:
    print("\nSUCCESS: Both captures worked!")
    exit(0)
elif first_ok and not second_ok:
    print("\nFAILURE: Issue #26 reproduced - second capture failed")
    exit(1)
else:
    print("\nFAILURE: Unexpected result")
    exit(1)
