#!/usr/bin/env python3
"""Replace only the terminal client; Fluent still owns its lease and PTY."""
import os
import select
import sys
import tty

tty.setraw(0)
os.write(1, b"\x1b[2J\x1b[HCLIENT_READY " + sys.argv[1].encode())
flood = os.environ.get("FLUENT_TEST_DASHBOARD_FLOOD") == "1"
while True:
    if flood:
        os.write(1, ("\x1b[Hflood 界\r\n" * 1024).encode())
    if not select.select([0], [], [], 0 if flood else None)[0]:
        continue
    data = os.read(0, 8192)
    if not data:
        break
    if sys.argv[1] == "control":
        with open(os.environ["FLUENT_TEST_TERMINAL_CLIENT_LOG"], "ab") as log:
            log.write(data)
        os.write(1, data)
