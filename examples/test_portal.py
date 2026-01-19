#!/usr/bin/env python3
"""Test the portal window selection flow."""

from pipewire_capture import PortalCapture, is_available

print(f"PipeWire capture available: {is_available()}")

if not is_available():
    print("Not running on Wayland, exiting")
    exit(1)

print("Opening window picker...")
portal = PortalCapture()
info = portal.select_window()

print(f"Stream info: {info}")

if info:
    print(f"  File descriptor: {info.fd}")
    print(f"  Node ID: {info.node_id}")
    print(f"  Window size: {info.width}x{info.height}")
    print("\nSuccess! Portal integration is working.")
else:
    print("\nNo stream info - selection was cancelled.")
