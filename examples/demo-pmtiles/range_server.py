#!/usr/bin/env python3
"""Mini static server with HTTP Range support (for PMTiles demo).

Usage: python3 range_server.py [port]
Serves the current directory. Supports single-range GET requests,
which is all the pmtiles JS client needs.
"""
import os
import sys
import email.utils
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import unquote, urlparse

ROOT = os.path.dirname(os.path.abspath(__file__))

MIME = {
    ".html": "text/html",
    ".js": "text/javascript",
    ".css": "text/css",
    ".png": "image/png",
    ".json": "application/json",
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        path = unquote(urlparse(self.path).path).lstrip("/")
        full = os.path.abspath(os.path.join(ROOT, path))
        if not full.startswith(ROOT) or not os.path.isfile(full):
            self.send_error(404)
            return
        size = os.path.getsize(full)
        mtime = email.utils.formatdate(os.path.getmtime(full), usegmt=True)
        ctype = MIME.get(os.path.splitext(full)[1].lower(), "application/octet-stream")

        rng = self.headers.get("Range")
        if rng and rng.startswith("bytes="):
            spec = rng[6:].split(",")[0].split("-")
            start = int(spec[0]) if spec[0] else 0
            end = int(spec[1]) if len(spec) > 1 and spec[1] else size - 1
            end = min(end, size - 1)
            length = end - start + 1
            self.send_response(206)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(length))
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Last-Modified", mtime)
            self.end_headers()
            with open(full, "rb") as f:
                f.seek(start)
                self.wfile.write(f.read(length))
        else:
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(size))
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Last-Modified", mtime)
            self.end_headers()
            with open(full, "rb") as f:
                self.wfile.write(f.read())

    def log_message(self, fmt, *args):
        sys.stderr.write(f"{self.address_string()} {fmt % args}\n")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8734
    print(f"serving {ROOT} on :{port} (range requests OK)")
    HTTPServer(("", port), Handler).serve_forever()
