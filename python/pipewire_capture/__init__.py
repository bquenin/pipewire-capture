"""PipeWire video capture for Python.

This library provides PipeWire-based video capture for Wayland,
using the xdg-desktop-portal ScreenCast interface for window selection.

Example usage:

    from pipewire_capture import PortalCapture, CaptureStream, is_available

    if is_available():
        # Window selection via portal
        portal = PortalCapture()
        session = portal.select_window()  # Returns PortalSession or None

        if session:
            # Frame capture
            stream = CaptureStream(session.fd, session.node_id,
                                   session.width, session.height)
            stream.start()

            frame = stream.get_frame()  # numpy array (H, W, 4) BGRA
            if frame is not None:
                print(f"Got frame: {frame.shape}")

            stream.stop()
            session.close()  # Close portal session when done
"""

from importlib.metadata import version

from pipewire_capture._native import (
    CaptureStream,
    PortalCapture,
    PortalSession,
    is_available,
)

__all__ = [
    "PortalCapture",
    "PortalSession",
    "CaptureStream",
    "is_available",
]

__version__ = version("pipewire-capture")
