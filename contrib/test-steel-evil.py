#!/usr/bin/env python3
"""Exercise the actual Steel/Evil editor through a PTY, without touching user config.

Run after cargo build: python3 contrib/test-steel-evil.py
Requires POSIX, Python 3, git and network on the first plugin download.
"""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = ROOT / "contrib/steel-evil/hx-evil"


def exercise(state, name, initial, keys, expected, burst=False):
    document = state / (name + ".txt")
    document.write_text(initial)
    pid, fd = pty.fork()
    if pid == 0:
        for key in tuple(os.environ):
            if key.startswith("MIRAGE_"):
                os.environ.pop(key)
        os.environ.update(
            TERM="xterm-256color",
            HELIX_STEEL_EVIL_HOME=str(state / "profile"),
            HX_BIN=str(ROOT / "target/debug/hx"),
        )
        os.execv(str(LAUNCHER), [str(LAUNCHER), str(document)])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
    output = bytearray()
    reaped = False

    def drain(duration):
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            ready, _, _ = select.select([fd], [], [], min(0.1, max(0, deadline - time.monotonic())))
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError as exc:
                if exc.errno == errno.EIO:
                    return
                raise
            if not data:
                return
            output.extend(data)
            # Minimal terminal query responses for the real terminal backend.
            if b"\x1b[6n" in data:
                os.write(fd, b"\x1b[1;1R")
            if b"\x1b[c" in data or b"\x1b[0c" in data:
                os.write(fd, b"\x1b[?1;2c")
            if b"\x1b]11;?" in data:
                os.write(fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")

    try:
        # First launch may fetch the pinned plugin. Wait for the actual editor,
        # not a fixed sleep that could type into git or a startup error.
        deadline = time.monotonic() + 90
        while b"NOR" not in output and time.monotonic() < deadline:
            drain(0.2)
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                reaped = True
                raise AssertionError(f"{name}: editor exited during startup ({status})")
        if b"NOR" not in output:
            raise AssertionError(f"{name}: editor did not become ready")
        drain(0.4)
        if burst:
            # Regression: the Steel interrupt watchdog must not consume ordinary
            # terminal events buffered behind a plugin command.
            os.write(fd, keys)
        else:
            for key in keys:
                os.write(fd, bytes([key]))
                drain(0.08)
        drain(0.7)
        os.write(fd, b":write-quit\r")
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            drain(0.1)
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                reaped = True
                assert os.waitstatus_to_exitcode(status) == 0, (name, status)
                break
        if not reaped:
            raise AssertionError(f"{name}: editor did not exit")
        actual = document.read_text()
        assert actual == expected, f"{name}: expected {expected!r}, got {actual!r}"
        print(f"PASS {name}", flush=True)
    except Exception:
        (state / (name + ".terminal.log")).write_bytes(output)
        print(f"Terminal transcript: {state / (name + '.terminal.log')}", flush=True)
        raise
    finally:
        if not reaped:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        os.close(fd)


def main():
    scratch = os.environ.get("JCODE_SCRATCH_DIR")
    state = Path(tempfile.mkdtemp(prefix="helix-evil-smoke-", dir=scratch))
    print(f"Isolated smoke test state: {state}", flush=True)
    cases = [
        ("delete-word", "alpha beta\n", b"dw", "beta\n"),
        ("delete-line", "alpha\nbeta\n", b"dd", "beta\n"),
        ("change-inner-word", "alpha beta\n", b"ciwchanged\x1b", "changed beta\n"),
        ("insert-escape", "alpha\n", b"ihello \x1b", "hello alpha\n"),
        ("burst-change-inner-word", "alpha beta\n", b"ciwchanged\x1b", "changed beta\n", True),
        ("burst-insert-escape", "alpha\n", b"ihello \x1b", "hello alpha\n", True),
    ]
    for case in cases:
        exercise(state, *case)
    print("All real-editor Steel/Evil smoke tests passed.")


if __name__ == "__main__":
    main()
