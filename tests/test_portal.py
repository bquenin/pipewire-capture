"""Exercise the native extension against a private, deterministic D-Bus portal.

Run with: uv run python -m unittest discover -s tests -v
No desktop session, screen-sharing permission or real PipeWire server is needed.
"""

import asyncio
from concurrent.futures import ThreadPoolExecutor
import gc
import os
import socket
import subprocess
import threading
import time
import unittest

from dbus_next import Message, MessageType, Variant
from dbus_next.aio import MessageBus

from pipewire_capture import CaptureStream, PortalCapture, UnsupportedCompositorError


class MockPortal:
    def __init__(self, address):
        self.address = address
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self.loop.run_forever, daemon=True)
        self.thread.start()
        self.remote = open("/dev/null", "rb")
        self.reset()
        asyncio.run_coroutine_threadsafe(self.connect(), self.loop).result(5)

    def reset(self):
        self.fail_stage = None
        self.response_code = 2
        self.backend = "gl2"
        self.ignore_compositor_queries = False
        self.events = []
        self.pending_start = None
        self.hold_start = False
        self.start_received = threading.Event()

    async def connect(self):
        self.bus = await MessageBus(bus_address=self.address, negotiate_unix_fd=True).connect()
        await self.bus.request_name("org.freedesktop.portal.Desktop")
        await self.bus.request_name("org.kde.KWin")
        self.bus.add_message_handler(self.handle)

    def respond(self, message, options, results):
        sender = message.sender.removeprefix(":").replace(".", "_")
        path = f"/org/freedesktop/portal/desktop/request/{sender}/{options['handle_token'].value}"
        self.bus.send(Message.new_method_return(message, "o", [path]))
        code = self.response_code if message.member == self.fail_stage else 0
        self.bus.send(Message(
            message_type=MessageType.SIGNAL,
            path=path,
            interface="org.freedesktop.portal.Request",
            member="Response",
            signature="ua{sv}",
            body=[code, {} if code else results],
        ))

    def release_start(self):
        def release():
            message, options, results = self.pending_start
            self.pending_start = None
            self.respond(message, options, results)
        self.loop.call_soon_threadsafe(release)

    def handle(self, message):
        if message.message_type != MessageType.METHOD_CALL:
            return False
        member = message.member
        if message.interface == "org.freedesktop.DBus.Properties":
            if message.path == "/Compositor" and self.ignore_compositor_queries:
                return True  # An unresponsive diagnostic service must not block capture errors.
            properties = {
                "version": Variant("u", 5),
                "AvailableSourceTypes": Variant("u", 2),
                "AvailableCursorModes": Variant("u", 3),
                "compositingType": Variant("s", self.backend),
            }
            if member == "Get":
                self.bus.send(Message.new_method_return(message, "v", [properties[message.body[1]]]))
            else:
                self.bus.send(Message.new_method_return(message, "a{sv}", [properties]))
            return True
        self.events.append(member)
        if member == "CreateSession":
            options = message.body[0]
            sender = message.sender.removeprefix(":").replace(".", "_")
            session = f"/org/freedesktop/portal/desktop/session/{sender}/{options['session_handle_token'].value}"
            self.respond(message, options, {"session_handle": Variant("s", session)})
        elif member == "SelectSources":
            self.respond(message, message.body[-1], {})
        elif member == "Start":
            options = message.body[-1]
            results = {"streams": Variant("a(ua{sv})", [[42, {"size": Variant("(ii)", [2, 2])}]])}
            if self.hold_start:
                self.pending_start = message, options, results
                self.start_received.set()
            else:
                self.respond(message, options, results)
        elif member == "OpenPipeWireRemote":
            if self.fail_stage == member:
                self.bus.send(Message.new_error(message, "org.freedesktop.portal.Error.Failed", "remote refused"))
            else:
                self.bus.send(Message.new_method_return(message, "h", [0], unix_fds=[self.remote.fileno()]))
        elif member == "Close":
            self.bus.send(Message.new_method_return(message))
        else:
            return False
        return True

    def close(self):
        self.loop.call_soon_threadsafe(self.bus.disconnect)
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(5)
        self.remote.close()


class PortalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.previous_address = os.environ.get("DBUS_SESSION_BUS_ADDRESS")
        cls.daemon = subprocess.Popen(
            ["dbus-daemon", "--session", "--nofork", "--print-address=1", "--nopidfile"],
            stdout=subprocess.PIPE, text=True,
        )
        address = cls.daemon.stdout.readline().strip()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = address
        cls.mock = MockPortal(address)

    @classmethod
    def tearDownClass(cls):
        cls.mock.close()
        cls.daemon.terminate()
        cls.daemon.wait(timeout=5)
        cls.daemon.stdout.close()
        if cls.previous_address is None:
            os.environ.pop("DBUS_SESSION_BUS_ADDRESS", None)
        else:
            os.environ["DBUS_SESSION_BUS_ADDRESS"] = cls.previous_address

    def setUp(self):
        self.mock.reset()

    def test_select_sources_failure_is_not_ignored_and_closes_session(self):
        self.mock.fail_stage = "SelectSources"
        self.mock.backend = "qpainter"
        with self.assertRaisesRegex(RuntimeError, "Portal SelectSources failed"):
            PortalCapture().select_window()
        self.assertEqual(self.mock.events, ["CreateSession", "SelectSources", "Close"])

    def test_cancelled_requests_return_none_and_close_created_sessions(self):
        self.mock.response_code = 1
        self.mock.backend = "qpainter"
        for stage in ("CreateSession", "SelectSources", "Start"):
            with self.subTest(stage=stage):
                self.mock.events = []
                self.mock.fail_stage = stage
                self.assertIsNone(PortalCapture().select_window())
                self.assertEqual("Close" in self.mock.events, stage != "CreateSession")

    def test_kde_failure_has_a_specific_python_exception(self):
        self.mock.fail_stage = "Start"
        for backend in ("qpainter", "none"):
            with self.subTest(backend=backend):
                self.mock.backend = backend
                with self.assertRaises(UnsupportedCompositorError) as caught:
                    PortalCapture().select_window()
                self.assertIsInstance(caught.exception, RuntimeError)
                self.assertIn("requires OpenGL", str(caught.exception))
                self.assertIn(backend, str(caught.exception))
                self.assertEqual(self.mock.events[-1], "Close")

    def test_missing_kwin_service_preserves_the_portal_error(self):
        self.mock.fail_stage = "Start"
        asyncio.run_coroutine_threadsafe(self.mock.bus.release_name("org.kde.KWin"), self.mock.loop).result(2)
        try:
            with self.assertRaisesRegex(RuntimeError, "Portal Start failed") as caught:
                PortalCapture().select_window()
            self.assertNotIsInstance(caught.exception, UnsupportedCompositorError)
        finally:
            asyncio.run_coroutine_threadsafe(self.mock.bus.request_name("org.kde.KWin"), self.mock.loop).result(2)

    def test_unresponsive_kwin_diagnostic_times_out_without_hiding_error(self):
        self.mock.fail_stage = "Start"
        self.mock.ignore_compositor_queries = True
        started = time.monotonic()
        with self.assertRaisesRegex(RuntimeError, "Portal Start failed") as caught:
            PortalCapture().select_window()
        self.assertNotIsInstance(caught.exception, UnsupportedCompositorError)
        self.assertLess(time.monotonic() - started, 3)

    def test_other_start_errors_are_not_misdiagnosed_as_compositor_failures(self):
        self.mock.fail_stage = "Start"
        for backend in ("gl2", "gles", "future-backend"):
            with self.subTest(backend=backend):
                self.mock.backend = backend
                with self.assertRaisesRegex(RuntimeError, "Portal Start failed") as caught:
                    PortalCapture().select_window()
                self.assertNotIsInstance(caught.exception, UnsupportedCompositorError)

    def test_open_remote_failure_closes_session_and_preserves_stage(self):
        self.mock.fail_stage = "OpenPipeWireRemote"
        with self.assertRaisesRegex(RuntimeError, "Portal OpenPipeWireRemote failed"):
            PortalCapture().select_window()
        self.assertEqual(self.mock.events[-1], "Close")

    def test_session_closes_fd_without_requiring_a_capture_stream(self):
        session = PortalCapture().select_window()
        fd = session.fd
        os.fstat(fd)
        self.assertTrue(session.is_open)
        session.close()
        session.close()  # idempotent
        self.assertFalse(session.is_open)
        self.assertEqual(session.fd, -1)
        with self.assertRaises(OSError):
            os.fstat(fd)

    def test_discarding_a_session_closes_its_fd_and_portal(self):
        session = PortalCapture().select_window()
        fd = session.fd
        del session
        gc.collect()
        with self.assertRaises(OSError):
            os.fstat(fd)
        deadline = time.monotonic() + 2
        while "Close" not in self.mock.events and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertIn("Close", self.mock.events)

    def test_close_is_not_blocked_by_another_window_picker(self):
        first = PortalCapture().select_window()
        self.mock.hold_start = True
        with ThreadPoolExecutor(max_workers=2) as pool:
            pending = pool.submit(PortalCapture().select_window)
            self.assertTrue(self.mock.start_received.wait(2))
            try:
                pool.submit(first.close).result(timeout=2)
            finally:
                self.mock.release_start()
            second = pending.result(timeout=2)
            second.close()


class StreamTests(unittest.TestCase):
    def test_stream_borrows_descriptor_and_releases_its_duplicate(self):
        with open("/dev/null", "rb") as original:
            baseline = len(os.listdir("/proc/self/fd"))
            stream = CaptureStream(original.fileno(), 42, 0, 0)
            self.assertEqual(len(os.listdir("/proc/self/fd")), baseline + 1)
            stream.stop()  # cleanup before start
            self.assertEqual(len(os.listdir("/proc/self/fd")), baseline)
            os.fstat(original.fileno())

    def test_invalid_fd_and_capture_interval_raise_regular_python_errors(self):
        with self.assertRaises(OSError):
            CaptureStream(-1, 42, 0, 0)
        with open("/dev/null", "rb") as original:
            for interval in (-1, float("nan"), float("inf")):
                with self.assertRaises(ValueError):
                    CaptureStream(original.fileno(), 42, 0, 0, interval)
                os.fstat(original.fileno())

    def test_disconnected_remote_becomes_visible_to_the_caller(self):
        client, server = socket.socketpair()
        with client, server:
            stream = CaptureStream(client.fileno(), 42, 0, 0)
            server.close()
            stream.start()
            try:
                deadline = time.monotonic() + 3
                while not stream.window_invalid and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(stream.window_invalid)
                self.assertTrue(stream.error)
                self.assertIsNone(stream.get_frame())
            finally:
                stream.stop()


if __name__ == "__main__":
    unittest.main()
