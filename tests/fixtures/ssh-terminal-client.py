#!/usr/bin/env -S python3 -I -S
"""Stand in for tmux while real Fluent owns the generation, lease and PTY."""
import os
import select
import sys
import tty
from collections import deque

tty.setraw(0)
os.set_blocking(0, False)
os.set_blocking(1, False)
output = deque([memoryview(b"\x1b[2J\x1b[HSSH_FULLSCREEN_PAINT\r\n# ")])
lines = int(os.environ.get("FLUENT_TEST_TERMINAL_OUTPUT_LINES", "0"))
if "FLUENT_SSH_FULLSCREEN_CHILD" in os.environ or lines:
    output.append(memoryview((b"X" * 98 + b"\r\n") * (lines or 1280)))
    output.append(memoryview(b"SSH_FULLSCREEN_PAINT_END\r\n"))
prefix = False
while True:
    readable, writable, _ = select.select([0], [1] if output else [], [])
    # Both repaint and echo must yield to input while the terminal is full.
    # A blocked echo must not prevent a later Ctrl-B d from detaching.
    if readable:
        try:
            data = os.read(0, 8192)
        except BlockingIOError:
            data = None
        if data == b"":
            break
        if data:
            if sys.argv[1] == "control":
                with open(os.environ['FLUENT_TEST_TERMINAL_CLIENT_LOG'], 'ab') as log:
                    log.write(data)
            for byte in data:
                if prefix and byte == ord('d'):
                    signals = os.environ.get("FLUENT_SSH_FULLSCREEN_SIGNALS")
                    if signals:
                        with open(os.path.join(signals, "client-detached"), "w") as receipt:
                            receipt.write("detached")
                    sys.exit(0)
                prefix = byte == 2
            if sys.argv[1] == "control":
                output.append(memoryview(data))
    if writable:
        try:
            written = os.write(1, output[0][:8192])
        except BlockingIOError:
            continue
        output[0] = output[0][written:]
        if not output[0]:
            output.popleft()
