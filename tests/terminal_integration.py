"""Run with: python3 tests/terminal_integration.py [path/to/az-pipeline-watch].

Uses a POSIX pseudo-terminal and a local az double, never the real Azure CLI.
"""

import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest


BINARY = Path(sys.argv.pop(1) if len(sys.argv) > 1 else
              "target/release/az-pipeline-watch").resolve()
URL = b"https://dev.azure.com/org/project/_build/results?buildId=123\n"


class TerminalSession:
    def __init__(self, *flags, api_error=False):
        self.directory = tempfile.TemporaryDirectory(prefix="watch-integration-")
        self.root = Path(self.directory.name)
        mock = self.root / "az"
        shutil.copyfile(Path(__file__).parent / "fixtures" / "az", mock)
        mock.chmod(0o755)
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ,
                    struct.pack("HHHH", 30, 140, 0, 0))
        self.settings = termios.tcgetattr(self.slave)
        env = dict(os.environ, PATH=f"{self.root}{os.pathsep}{os.environ['PATH']}",
                   WATCH_TEST_STATE=str(self.root),
                   WATCH_TEST_API_ERROR="1" if api_error else "0")
        self.process = subprocess.Popen(
            [str(BINARY), *flags], stdin=self.slave, stdout=self.slave,
            stderr=self.slave, env=env, close_fds=True,
        )
        self.output = bytearray()
        self.position = 0

    def __enter__(self):
        return self

    def __exit__(self, *_):
        try:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=3)
        finally:
            os.close(self.master)
            os.close(self.slave)
            self.directory.cleanup()

    def send(self, data):
        os.write(self.master, data)

    def read(self, timeout=0.1):
        if select.select([self.master], [], [], timeout)[0]:
            try:
                self.output.extend(os.read(self.master, 65536))
            except OSError as error:
                if error.errno != errno.EIO:
                    raise

    def expect(self, text):
        marker = text.encode()
        deadline = time.monotonic() + 15
        while True:
            index = self.output.find(marker, self.position)
            if index != -1:
                self.position = index + len(marker)
                return
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"Did not see {text!r}. Output:\n"
                    f"{self.output.decode(errors='replace')}"
                )
            self.read()

    def choose_uat(self):
        self.expect("Azure DevOps build URL:")
        self.send(URL)
        self.expect("Select stages to approve")
        self.expect("> [ ] Build")
        self.send(b"\x1b[B")
        self.expect("> [ ] UAT")
        self.send(b" ")
        self.expect("> [x] UAT")
        self.send(b"\r")
        self.expect("Type 'approve'")

    def finish(self, expected_code=0):
        deadline = time.monotonic() + 15
        while self.process.poll() is None and time.monotonic() < deadline:
            self.read()
        if self.process.poll() is None:
            raise AssertionError("Process did not exit")
        while select.select([self.master], [], [], 0.1)[0]:
            self.read(0)
        if self.process.returncode != expected_code:
            raise AssertionError(self.output.decode(errors="replace"))
        if termios.tcgetattr(self.slave) != self.settings:
            raise AssertionError("Terminal settings were not restored")
        if b"\x1b[?25l" not in self.output or b"\x1b[?25h" not in self.output:
            raise AssertionError("Cursor was not hidden and restored")

    def votes(self):
        path = self.root / "votes.jsonl"
        return [vote for line in path.read_text().splitlines()
                for vote in json.loads(line)] if path.exists() else []


class TerminalIntegrationTests(unittest.TestCase):
    def test_selects_pending_stage_and_approves_only_uat(self):
        with TerminalSession("--interval", "1") as session:
            session.choose_uat()
            self.assertEqual(session.votes(), [])
            session.send(b"approve\n")
            session.expect("Confirmed approval vote for UAT")
            # A second cycle must not repeat a vote already submitted.
            session.send(b"r")
            session.expect("Approval UAT-approval [UAT]: approved")
            session.send(b"\x03")
            session.finish()
            self.assertEqual(
                [vote["approvalId"] for vote in session.votes()], ["UAT-approval"]
            )
            self.assertIn("stage=UAT", session.votes()[0]["comment"])

    def test_dry_run_never_submits_patch(self):
        with TerminalSession("--dry-run", "--once") as session:
            session.choose_uat()
            session.send(b"approve\n")
            session.expect("DRY RUN: would approve UAT-approval for UAT")
            session.finish()
            self.assertEqual(session.votes(), [])

    def test_escape_cancels_without_approving(self):
        with TerminalSession() as session:
            session.expect("Azure DevOps build URL:")
            session.send(URL)
            session.expect("Select stages to approve")
            session.send(b"\x1b")
            session.expect("Selection cancelled.")
            session.finish()
            self.assertEqual(session.votes(), [])

    def test_declining_confirmation_does_not_approve(self):
        with TerminalSession() as session:
            session.choose_uat()
            session.send(b"no\n")
            session.expect("Approval cancelled.")
            session.finish()
            self.assertEqual(session.votes(), [])

    def test_api_error_is_reported_and_restores_terminal(self):
        with TerminalSession(api_error=True) as session:
            session.choose_uat()
            session.send(b"approve\n")
            session.expect("Simulated Azure API failure")
            session.finish(expected_code=1)
            self.assertEqual(session.votes(), [])

    def test_refresh_interval_and_ctrl_c(self):
        with TerminalSession("--dry-run", "--interval", "30") as session:
            session.choose_uat()
            session.send(b"approve\n")
            session.expect("Refresh interval: 30s")
            session.expect("DRY RUN: would approve UAT-approval for UAT")
            session.send(b"+")
            session.expect("Refresh interval: 25s")
            session.send(b"-")
            session.expect("Refresh interval: 30s")
            session.send(b"r")
            session.expect("DRY RUN: would approve UAT-approval for UAT")
            session.send(b"\x03")
            session.expect("Monitoring stopped.")
            session.finish()
            self.assertEqual(session.votes(), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
