#!/usr/bin/env python3
"""Real-editor MCP smoke test through a PTY with an isolated HOME/config.

Run after cargo build:
  HELIX_MCP_URL=http://127.0.0.1:17345/mcp \
  HELIX_MCP_TOKEN_FILE=/path/to/token \
  python3 contrib/test-mcp.py

It only passes the token file *path* to the editor's config. The token is
never read, printed, or written by this script. The Mirage server should
expose read-only docs tools (docs_get, docs_outline, docs_search).
The evil-profile scenario uses contrib/steel-evil/hx-evil (fetches vim.hx on
first run, needs git/network). Set HELIX_MCP_SKIP_EVIL=1 to skip it.
All Steel/XDG/HELIX env is isolated under a temp dir, removed on success.
"""
import errno
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]
HX = Path(os.environ.get("HX_BIN", ROOT / "target/debug/hx"))
URL = os.environ.get("HELIX_MCP_URL", "http://127.0.0.1:17345/mcp")
TOKEN_FILE = os.environ.get("HELIX_MCP_TOKEN_FILE")
LAUNCHER = ROOT / "contrib/steel-evil/hx-evil"
F11 = b"\x1b[23~"
F12 = b"\x1b[24~"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][0-9A-B]|\x1b[=>78]")


def squash(s):
    return re.sub(r"\s+", "", s)


CSI = re.compile(rb"\x1b\[([0-9;?:<=>]*)([ -/]*)([@-~])|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1bP.*?\x1b\\|\x1b[()][0-9A-B]|\x1b[=>78]", re.S)


class Screen:
    """Tiny terminal emulator: enough of CUP/ED/EL/cursor moves for crossterm diffs."""

    def __init__(self, rows=40, cols=200):
        self.rows, self.cols = rows, cols
        self.grid = [[" "] * cols for _ in range(rows)]
        self.r = self.c = 0
        self.pending = b""

    def feed(self, data):
        data = self.pending + data
        self.pending = b""
        i = 0
        while i < len(data):
            b = data[i]
            if b == 0x1b:
                m = CSI.match(data, i)
                if not m:
                    if len(data) - i < 64:
                        self.pending = data[i:]
                        return
                    i += 1
                    continue
                if m.group(3):
                    self.csi(m.group(1).decode(), m.group(3).decode())
                i = m.end()
                continue
            if b == 0x0d:
                self.c = 0
            elif b == 0x0a:
                self.r = min(self.r + 1, self.rows - 1)
            elif b == 0x08:
                self.c = max(self.c - 1, 0)
            elif b >= 0x20:
                n = 1 if b < 0x80 else (2 if b >= 0xF0 or b < 0xE0 else 3) if b >= 0xC0 else 1
                if b >= 0xF0:
                    n = 4
                if i + n > len(data):
                    self.pending = data[i:]
                    return
                ch = data[i:i + n].decode("utf-8", "replace")
                if self.c < self.cols:
                    self.grid[self.r][self.c] = ch
                self.c += 1
                i += n
                continue
            i += 1

    def csi(self, params, final):
        if params.startswith(("?", "<", ">", "=")):
            return
        nums = [int(p) if p.isdigit() else 0 for p in params.replace(":", ";").split(";")] if params else []
        a = nums[0] if nums else 0
        if final in "Hf":
            self.r = min(max((nums[0] if nums else 1) - 1, 0), self.rows - 1)
            self.c = min(max((nums[1] if len(nums) > 1 else 1) - 1, 0), self.cols - 1)
        elif final == "G":
            self.c = max(a - 1, 0)
        elif final == "d":
            self.r = min(max(a - 1, 0), self.rows - 1)
        elif final == "A":
            self.r = max(self.r - max(a, 1), 0)
        elif final == "B":
            self.r = min(self.r + max(a, 1), self.rows - 1)
        elif final == "C":
            self.c = min(self.c + max(a, 1), self.cols - 1)
        elif final == "D":
            self.c = max(self.c - max(a, 1), 0)
        elif final == "J":
            if a == 2 or a == 3:
                self.grid = [[" "] * self.cols for _ in range(self.rows)]
            elif a == 0:
                self.grid[self.r][self.c:] = [" "] * (self.cols - self.c)
                for r in range(self.r + 1, self.rows):
                    self.grid[r] = [" "] * self.cols
        elif final == "K":
            if a == 0:
                self.grid[self.r][self.c:] = [" "] * (self.cols - self.c)
            elif a == 2:
                self.grid[self.r] = [" "] * self.cols

    def text(self):
        return "\n".join("".join(row) for row in self.grid)


class Editor:
    def __init__(self, state, name, mcp_toml, env=None, evil=False):
        self.name = name
        self.home = state / name
        # hx-evil maps XDG_CONFIG_HOME to <state>/config, raw hx uses <home>/.config.
        xdg_config = self.home / ("config" if evil else ".config")
        cfg = xdg_config / "helix"
        cfg.mkdir(parents=True)
        (cfg / "mcp.toml").write_text(mcp_toml)
        self.log = state / f"{name}.terminal.log"
        self.output = bytearray()
        self.screen = Screen()
        self.mark = 0
        self.reaped = False
        child_env = {
            k: v for k, v in os.environ.items()
            if not k.startswith(("MIRAGE_", "XDG_", "HELIX_", "STEEL_", "HX_"))
        }
        steel = self.home / "steel"
        # Steel writes generated modules here and panics if missing (hx-evil does the same).
        for d in (steel / "cogs/helix", steel / "lsp", self.home / ".cache",
                  self.home / ".local/share", self.home / ".local/state"):
            d.mkdir(parents=True, exist_ok=True)
        child_env.update(
            TERM="xterm-256color",
            HOME=str(self.home),
            XDG_CONFIG_HOME=str(xdg_config),
            XDG_CACHE_HOME=str(self.home / ".cache"),
            XDG_DATA_HOME=str(self.home / ".local/share"),
            XDG_STATE_HOME=str(self.home / ".local/state"),
            HELIX_STEEL_CONFIG=str(cfg),
            STEEL_HOME=str(steel),
            STEEL_LSP_HOME=str(steel / "lsp"),
            HELIX_RUNTIME=str(ROOT / "runtime"),
        )
        child_env.update(env or {})
        argv = [str(HX)]
        if evil:
            child_env.update(HELIX_STEEL_EVIL_HOME=str(self.home), HX_BIN=str(HX))
            argv = [str(LAUNCHER)]
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(self.home)
            os.execve(argv[0], argv, child_env)
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
            self.screen.feed(data)
            if b"\x1b[6n" in data:
                os.write(self.fd, b"\x1b[1;1R")
            if b"\x1b[c" in data or b"\x1b[0c" in data:
                os.write(self.fd, b"\x1b[?1;2c")
            if b"\x1b]11;?" in data:
                os.write(self.fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")

    def text(self, since=0):
        # Current screen contents, whitespace squashed so wrapped/padded
        # status lines still match. `since` is unused (screen is a snapshot).
        return squash(self.screen.text())

    def wait_for(self, needles, timeout=20, since=None, forbid=()):
        since = self.mark if since is None else since
        needles = [needles] if isinstance(needles, str) else needles
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain(0.2)
            t = self.text(since)
            for f in forbid:
                if squash(f) in t:
                    raise AssertionError(f"{self.name}: unexpected {f!r} on screen")
            if all(squash(n) in t for n in needles):
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

    def ready(self, timeout):
        self.wait_for("NOR", timeout=timeout, since=0)
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


def run(state, name, mcp_toml, body, env=None, evil=False):
    ed = Editor(state, name, mcp_toml, env, evil)
    failed = True
    try:
        ed.ready(90 if evil else 30)
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
        if squash(want) in ed.text(ed.mark):
            return
        ed.drain(0.6)
    raise AssertionError(f"{ed.name}: status never showed {want!r}")


def main_flow(ed):
    # Auto-connect with no manual command.
    poll_status(ed, "mirage: connected")
    ed.cmd("mcp-tools")
    ed.wait_for(["docs_get", "docs_outline", "docs_search"])
    ed.cmd("mcp-call mirage docs_outline {}")
    ed.wait_for(["# MCP mirage/docs_outline", "sections"], forbid=("MCP mirage/docs_outline: ",))
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


def evil_keys_flow(ed):
    # F11/F12 must survive the vim.hx keymap replacement.
    poll_status(ed, "mirage: connected")
    ed.send(b"\x1b")
    ed.send(F11)
    ed.wait_for("Connect Mirage")
    ed.send(b"\x1b")
    ed.drain(0.3)
    ed.cmd("mcp-disconnect mirage")
    ed.wait_for("MCP mirage: disconnected")
    ed.send(F12)
    poll_status(ed, "mirage: connected")


def disabled_flow(ed):
    ed.drain(3)
    ed.cmd("mcp-status")
    t = ed.wait_for("mirage")
    ed.drain(0.5)
    t = ed.text(ed.mark)
    assert squash("mirage: connected") not in t, "auto_connect=false was ignored"


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
    if os.environ.get("HELIX_MCP_SKIP_EVIL") != "1":
        run(state, "evil-profile-keys", dev_toml(), evil_keys_flow, evil=True)
    # Only our own mkdtemp dir; kept on failure for transcripts.
    shutil.rmtree(state)
    print("All real-editor MCP smoke tests passed (isolated state removed).")


if __name__ == "__main__":
    main()
