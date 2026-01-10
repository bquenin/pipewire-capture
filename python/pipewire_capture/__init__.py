"""PipeWire video capture for Python.

This library provides PipeWire-based video capture for Wayland,
using the xdg-desktop-portal ScreenCast interface for window selection.

Example usage:

    from pipewire_capture import PortalCapture, CaptureStream, is_available

    if is_available():
        # Window selection via portal
        portal = PortalCapture()
        portal.select_window(lambda success: print(f"Selected: {success}"))

        stream_info = portal.get_stream_info()
        if stream_info:
            fd, node_id = stream_info

            # Frame capture
            stream = CaptureStream(fd, node_id)
            stream.start()

            frame = stream.get_frame()  # numpy array (H, W, 4) BGRA
            if frame is not None:
                print(f"Got frame: {frame.shape}")

            stream.stop()

        portal.close()
"""

from pipewire_capture._native import (
    CaptureStream,
    PortalCapture,
    is_available,
)

__all__ = [
    "PortalCapture",
    "CaptureStream",
    "is_available",
]

__version__ = "0.2.0"
