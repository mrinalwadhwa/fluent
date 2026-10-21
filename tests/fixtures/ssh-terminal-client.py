#!/usr/bin/env python3
"""Stand in for tmux while real Fluent owns the generation, lease and PTY."""
import os
import sys
import tty

tty.setraw(0)
os.write(1, b"\x1b[2J\x1b[HSSH_FULLSCREEN_PAINT\r\n# ")
if "FLUENT_SSH_FULLSCREEN_CHILD" in os.environ:
    # A native interface can repaint more than the local PTY output buffer.
    paint = (b"X" * 98 + b"\r\n") * 1280
    while paint:
        paint = paint[os.write(1, paint):]
    os.write(1, b"SSH_FULLSCREEN_PAINT_END\r\n")
prefix = False
while True:
    data = os.read(0, 8192)
    if not data:
        break
    for byte in data:
        if prefix and byte == ord('d'):
            sys.exit(0)
        prefix = byte == 2
    if sys.argv[1] == "control":
        with open(os.environ['FLUENT_TEST_TERMINAL_CLIENT_LOG'], 'ab') as log:
            log.write(data)
        os.write(1, data)
