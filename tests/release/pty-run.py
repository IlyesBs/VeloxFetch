"""Runs a command in a pseudo-terminal and types a passphrase whenever it is asked for one, the way the owner does at
a real terminal (the release tools read passphrases from a terminal only, never from a pipe). Test helper for
tests/release/publish-kit.test.mjs; never used to sign a real release.

    python3 -I tests/release/pty-run.py <passphrase> <timeout seconds> <command> [args...]

Prints everything the command printed and exits with its exit code (124 on timeout).
"""

import os
import pty
import re
import select
import sys
import time

passphrase, limit, argv = sys.argv[1], float(sys.argv[2]), sys.argv[3:]
pid, fd = pty.fork()
if pid == 0:
    os.execvp(argv[0], argv)

out = b""
answered_at = 0  # how much output there was when the last prompt was answered
deadline = time.monotonic() + limit
while True:
    if time.monotonic() > deadline:
        os.kill(pid, 9)
        sys.stdout.write(out.decode("utf-8", "replace"))
        sys.exit(124)
    ready, _, _ = select.select([fd], [], [], 0.2)
    if ready:
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        out += chunk
        # A prompt is the last line printed, a question about a passphrase ending with ": " (update-key.mjs's askHidden).
        last = out[answered_at:].decode("utf-8", "replace").split("\n")[-1]
        if re.search(r"[Pp]assphrase[^\n]*: ?$", last):
            answered_at = len(out)
            os.write(fd, passphrase.encode() + b"\r")

_, status = os.waitpid(pid, 0)
sys.stdout.write(out.decode("utf-8", "replace"))
sys.exit(os.waitstatus_to_exitcode(status))
