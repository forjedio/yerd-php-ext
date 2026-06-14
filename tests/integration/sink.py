#!/usr/bin/env python3
"""Tiny loopback TCP sink for integration tests.

Binds 127.0.0.1:<port>, accepts connections, and appends every received
newline-delimited JSON frame to an output file. Exits after <seconds> of
inactivity (or when no connection arrives). Used by smoke.sh to capture the
frames a PHP process emits through the extension.
"""
import json
import socket
import sys

port = int(sys.argv[1]) if len(sys.argv) > 1 else 2304
out_path = sys.argv[2] if len(sys.argv) > 2 else "/tmp/yerd-frames.log"
timeout = float(sys.argv[3]) if len(sys.argv) > 3 else 6.0

srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port))
srv.listen(8)
srv.settimeout(timeout)

with open(out_path, "w") as out:
    try:
        while True:
            conn, _ = srv.accept()
            conn.settimeout(2.0)
            buf = b""
            try:
                while True:
                    chunk = conn.recv(65536)
                    if not chunk:
                        break
                    buf += chunk
            except socket.timeout:
                pass
            out.write(buf.decode("utf-8", "replace"))
            out.flush()
            conn.close()
    except socket.timeout:
        pass

# Validate that every captured line is well-formed JSON (fail loudly if not).
with open(out_path) as f:
    for line in f:
        line = line.strip()
        if line:
            json.loads(line)
