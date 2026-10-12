#!/usr/bin/env python3
"""Real-editor MCP smoke test through a PTY with an isolated HOME/config.

Run after cargo build:
  HELIX_MCP_URL=http://127.0.0.1:17345/mcp \
  HELIX_MCP_TOKEN_FILE=/path/to/token \
  python3 contrib/test-mcp.py

It only passes the token file *path* to the editor's config. The token is
never read, printed, or written by this script. The Mirage server should
expose read-only docs tools (docs_get, docs_outline, docs_search).
"""
import errno
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]
HX = Path(os.environ.get("HX_BIN", ROOT / "target/debug/hx"))
URL = os.environ.get("HELIX_MCP_URL", "http://127.0.0.1:17345/mcp")
TOKEN_FILE = os.environ.get("HELIX_MCP_TOKEN_FILE")
F11 = b"\x1b[23~"
F12 = b"\x1b[24~"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][0-9A-B]|\x1b[=>78]")


class Editor:
    def __init__(self, state, name, mcp_toml, env=None):
        self.name = name
        self.home = state / name
        cfg = self.home / ".config/helix"
        cfg.mkdir(parents=True)
        (cfg / "mcp.toml").write_text(mcp_toml)
        self.log = state / f"{name}.terminal.log"
        self.output = bytearray()
        self.mark = 0
        self.reaped = False
        child_env = {
            k: v for k, v in os.environ.items()
            if not k.startswith(("MIRAGE_", "XDG_"))
        }
        child_env.update(TERM="xterm-256color", HOME=str(self.home),
                         XDG_CONFIG_HOME=str(self.home / ".config"),
                         XDG_CACHE_HOME=str(self.home / ".cache"),
                         XDG_DATA_HOME=str(self.home / ".local/share"))
        child_env.update(env or {})
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(self.home)
            os.execve(str(HX), [str(HX)], child_env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 200, 0, 0))

    def drain(self, duration):
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            r, _, _ = select.select([self.fd], [], [], min(0.1, max(0, deadline - time.monotonic())))
            if not r:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError as exc:
                if exc.errno == errno.EIO:
                    return
                raise
            if not data:
                return
            self.output.extend(data)
            if b"\x1b[6n" in data:
                os.write(self.fd, b"\x1b[1;1R")
            if b"\x1b[c" in data or b"\x1b[0c" in data:
                os.write(self.fd, b"\x1b[?1;2c")
            if b"\x1b]11;?" in data:
                os.write(self.fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")

    def text(self, since=0):
        return ANSI.sub(b"", bytes(self.output[since:])).decode("utf-8", "replace")

    def wait_for(self, needles, timeout=20, since=None, forbid=()):
        since = self.mark if since is None else since
        needles = [needles] if isinstance(needles, str) else needles
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain(0.2)
            t = self.text(since)
            for f in forbid:
                if f in t:
                    raise AssertionError(f"{self.name}: unexpected {f!r} on screen")
            if all(n in t for n in needles):
                return t
            if os.waitpid(self.pid, os.WNOHANG)[0]:
                self.reaped = True
                raise AssertionError(f"{self.name}: editor exited")
        raise AssertionError(f"{self.name}: timed out waiting for {needles!r}")

    def send(self, data):
        self.mark = len(self.output)
        os.write(self.fd, data)
        self.drain(0.15)

    def cmd(self, line):
        self.send(b"\x1b")
        self.send(b":" + line.encode() + b"\r")

    def ready(self):
        self.wait_for("NOR", timeout=30, since=0)
        self.drain(0.3)

    def close(self, failed):
        if not self.reaped:
            try:
                os.write(self.fd, b"\x1b:quit-all!\r")
                self.drain(1.5)
            except OSError:
                pass
            if not os.waitpid(self.pid, os.WNOHANG)[0]:
                os.kill(self.pid, signal.SIGKILL)
                os.waitpid(self.pid, 0)
        os.close(self.fd)
        if failed:
            self.log.write_bytes(self.output)
            print(f"Terminal transcript: {self.log}", flush=True)


def run(state, name, mcp_toml, body, env=None):
    ed = Editor(state, name, mcp_toml, env)
    failed = True
    try:
        ed.ready()
        body(ed)
        failed = False
        print(f"PASS {name}", flush=True)
    finally:
        ed.close(failed)


def dev_toml(auto=True):
    return (f'mirage_mode = "dev"\n[servers.mirage]\nurl = "{URL}"\n'
            f'token_file = "{TOKEN_FILE}"\nauto_connect = {"true" if auto else "false"}\n')


def poll_status(ed, want, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ed.cmd("mcp-status")
        ed.drain(0.6)
        if want in ed.text(ed.mark):
            return
        ed.drain(0.6)
    raise AssertionError(f"{ed.name}: status never showed {want!r}")


def main_flow(ed):
    # Auto-connect with no manual command.
    poll_status(ed, "mirage: connected")
    ed.cmd("mcp-tools")
    ed.wait_for(["docs_get", "docs_outline", "docs_search"])
    ed.cmd("mcp-call mirage docs_outline {}")
    t = ed.wait_for("MCP mirage/docs_outline", forbid=("MCP mirage/docs_outline: ",))
    ed.drain(0.5)
    assert len(ed.text(ed.mark).strip()) > 60, "docs_outline produced no content"
    ed.send(b"\x1b")
    ed.send(F11)
    ed.wait_for("Connect Mirage")
    ed.send(b"\x1b")
    ed.drain(0.3)
    ed.cmd("mcp-disconnect mirage")
    ed.wait_for("MCP mirage: disconnected")
    ed.send(F12)
    ed.wait_for("MCP mirage:")
    poll_status(ed, "mirage: connected")


def prod_flow(ed):
    # Auto-connect to prod should refuse without the access key, without crashing.
    ed.wait_for("access key required", since=0)
    ed.cmd("mcp-connect mirage-prod")
    ed.wait_for("access key required")
    ed.cmd("mcp-status")
    ed.wait_for("mirage-prod: disconnected")


def disabled_flow(ed):
    ed.drain(3)
    ed.cmd("mcp-status")
    t = ed.wait_for("mirage")
    ed.drain(0.5)
    t = ed.text(ed.mark)
    assert "mirage: connected" not in t, "auto_connect=false was ignored"


def main():
    if not TOKEN_FILE:
        raise SystemExit("set HELIX_MCP_TOKEN_FILE (path only; it is never read here)")
    if not HX.exists():
        raise SystemExit(f"build first: {HX} missing")
    state = Path(tempfile.mkdtemp(prefix="helix-mcp-smoke-", dir=os.environ.get("JCODE_SCRATCH_DIR")))
    print(f"Isolated MCP smoke state: {state}", flush=True)
    run(state, "autoconnect-tools-call-keys", dev_toml(), main_flow)
    prod = ('mirage_mode = "prod"\n[servers.mirage]\nauto_connect = false\n'
            '[servers.mirage-prod]\nurl = "https://mirage.example.invalid/mcp"\n'
            'token_env = "MIRAGE_ACCESS_KEY"\nrequire_auth = true\n')
    run(state, "prod-missing-key-refused", prod, prod_flow)
    run(state, "autoconnect-disabled", dev_toml(auto=False), disabled_flow)
    print("All real-editor MCP smoke tests passed.")


if __name__ == "__main__":
    main()
