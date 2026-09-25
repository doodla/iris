#!/usr/bin/env python3
"""A local stand-in for the OpenAI and Gemini APIs, for tests/live/mock-run.sh.

Usage: mock_providers.py STATE_DIR

Listens on 127.0.0.1 only, on a free port that it writes to STATE_DIR/port once
it is ready, and appends one JSON line per request it receives to
STATE_DIR/requests.jsonl: {"method", "route", "auth"}. "route" names the API
call (or is "unknown"), and "auth" says which credential header was present
(never its value). It answers just enough of each API for
scripts/live-verify.sh to pass every step with the cheapest settings:

    POST /v1/images/generations, /v1/images/edits   one 1024x1024 PNG (OpenAI)
    POST /v1/models/<model>:generateContent          one 512x512 PNG (Gemini)
    POST /v1beta/models/<model>:predictLongRunning   a Veo operation name
    GET  /v1beta/models/<model>/operations/<id>      that operation, already done
    GET  /v1beta/files/<id>:download                 a 4-second MP4

Anything else, including a proxy's CONNECT, is recorded as "unknown" and
answered with 404, so a test can assert that nothing unexpected was asked.
"""

import base64
import http.server
import json
import os
import struct
import sys
import threading
import time
import zlib

STATE = ""
LOCK = threading.Lock()
FILE_ID = "mock-video-1"


def png(width, height):
    """A valid, solid-color RGB PNG."""

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    row = b"\x00" + b"\x20\x80\xc0" * width
    idat = zlib.compress(row * height, 9)
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def mp4(seconds):
    """A minimal valid MP4: ftyp, moov with an mvhd of the given duration, mdat."""

    def box(kind, payload):
        return struct.pack(">I", 8 + len(payload)) + kind + payload

    ftyp = b"isom" + struct.pack(">I", 0) + b"isomiso2mp41"
    mvhd = bytes(4) + struct.pack(">IIII", 0, 0, 1000, seconds * 1000) + bytes(80)
    moov = box(b"moov", box(b"mvhd", mvhd) + box(b"trak", box(b"tkhd", bytes(84))))
    return box(b"ftyp", ftyp) + moov + box(b"mdat", b"\xab" * 2048)


OPENAI_PNG = base64.b64encode(png(1024, 1024)).decode()
GEMINI_PNG = base64.b64encode(png(512, 512)).decode()
VIDEO = mp4(4)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(length)
        path = self.path.split("?", 1)[0]
        if path in ("/v1/images/generations", "/v1/images/edits"):
            route = "openai." + path.rsplit("/", 1)[1]
            return self.answer(route, 200, {
                "created": int(time.time()),
                "data": [{"b64_json": OPENAI_PNG}],
                "output_format": "png",
                "usage": {"input_tokens": 50, "output_tokens": 196, "total_tokens": 246,
                          "input_tokens_details": {"text_tokens": 50, "image_tokens": 0}},
            })
        if path.startswith("/v1/models/") and path.endswith(":generateContent"):
            return self.answer("gemini.generateContent", 200, {
                "candidates": [{"content": {"role": "model", "parts": [
                    {"inlineData": {"mimeType": "image/png", "data": GEMINI_PNG}}]},
                    "finishReason": "STOP", "index": 0}],
                "usageMetadata": {"promptTokenCount": 12, "candidatesTokenCount": 1120,
                                  "totalTokenCount": 1132},
            })
        if path.startswith("/v1beta/models/") and path.endswith(":predictLongRunning"):
            model = path[len("/v1beta/models/"):-len(":predictLongRunning")]
            return self.answer("veo.submit", 200, {"name": f"models/{model}/operations/mock-op-1"})
        return self.answer("unknown", 404, {"error": {"code": 404, "message": "not mocked"}})

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path.startswith("/v1beta/models/") and "/operations/" in path:
            host, port = self.server.server_address[:2]
            uri = f"http://{host}:{port}/v1beta/files/{FILE_ID}:download?alt=media"
            return self.answer("veo.poll", 200, {
                "name": path[len("/v1beta/"):],
                "done": True,
                "response": {
                    "@type": "type.googleapis.com/google.ai.generativelanguage.v1beta.PredictLongRunningResponse",
                    "generateVideoResponse": {"generatedSamples": [{"video": {"uri": uri}}]},
                },
            })
        if path == f"/v1beta/files/{FILE_ID}:download":
            self.record("veo.download")
            self.send_response(200)
            self.send_header("Content-Type", "video/mp4")
            self.send_header("Content-Length", str(len(VIDEO)))
            self.end_headers()
            self.wfile.write(VIDEO)
            return None
        return self.answer("unknown", 404, {"error": {"code": 404, "message": "not mocked"}})

    def do_CONNECT(self):
        return self.answer("unknown", 404, {"error": {"code": 404, "message": "not a proxy"}})

    def answer(self, route, status, doc):
        self.record(route)
        body = json.dumps(doc).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def record(self, route):
        auth = []
        if self.headers.get("Authorization", "").startswith("Bearer "):
            auth.append("bearer")
        if self.headers.get("x-goog-api-key"):
            auth.append("x-goog-api-key")
        line = json.dumps({"method": self.command, "route": route, "auth": "+".join(auth) or "none"})
        with LOCK, open(os.path.join(STATE, "requests.jsonl"), "a") as f:
            f.write(line + "\n")

    def log_message(self, fmt, *args):
        sys.stderr.write("%s\n" % (fmt % args))


def exit_with_parent():
    """Stop serving if the test that started us goes away."""
    parent = os.getppid()
    while os.getppid() == parent:
        time.sleep(1)
    os._exit(0)


def main():
    global STATE
    STATE = sys.argv[1]
    open(os.path.join(STATE, "requests.jsonl"), "w").close()
    threading.Thread(target=exit_with_parent, daemon=True).start()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    tmp = os.path.join(STATE, "port.tmp")
    with open(tmp, "w") as f:
        f.write(str(server.server_address[1]))
    os.rename(tmp, os.path.join(STATE, "port"))
    server.serve_forever()


if __name__ == "__main__":
    main()
