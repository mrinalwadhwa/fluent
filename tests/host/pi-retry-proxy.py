#!/usr/bin/env python3
"""Return one retryable response, then relay Pi's local model requests."""

import http.client
import http.server
import json
import os
import pathlib
import threading
from urllib.parse import urlsplit

target = urlsplit(os.environ["FLUENT_NATIVE_PI_ENDPOINT"])
output = pathlib.Path(os.environ["FLUENT_NATIVE_PI_PROXY_OUTPUT"])
lock = threading.Lock()
requests = 0


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, _format, *_args):
        pass

    def do_GET(self):
        upstream = http.client.HTTPConnection(target.hostname, target.port, timeout=30)
        upstream.request("GET", self.path, headers={"Host": target.netloc})
        response = upstream.getresponse()
        payload = response.read()
        self.send_response(response.status)
        for key, value in response.getheaders():
            if key.lower() not in {"connection", "content-length", "transfer-encoding"}:
                self.send_header(key, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
        upstream.close()

    def do_POST(self):
        global requests
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        rejected = os.environ.get("FLUENT_NATIVE_PI_REJECTED_TEXT", "").encode()
        if rejected and rejected in body:
            (output / "rejected-seen.txt").write_text(self.path)
        with lock:
            requests += 1
            number = requests
            (output / "request-count.txt").write_text(str(number))
        if number == 1:
            payload = json.dumps({"error": {"message": "temporary model overload", "type": "server_error"}}).encode()
            self.send_response(503)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            (output / "retry-triggered.txt").write_text(self.path)
            return

        upstream = http.client.HTTPConnection(target.hostname, target.port, timeout=600)
        headers = {key: value for key, value in self.headers.items()
                   if key.lower() not in {"host", "connection", "content-length"}}
        headers["Host"] = target.netloc
        upstream.request("POST", self.path, body, headers)
        response = upstream.getresponse()
        payload = response.read()
        self.send_response(response.status)
        for key, value in response.getheaders():
            if key.lower() not in {"connection", "content-length", "transfer-encoding"}:
                self.send_header(key, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
        upstream.close()


with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
    (output / "port.txt").write_text(str(server.server_address[1]))
    server.serve_forever()
