#!/usr/bin/env python3
"""Local stand-in for GitHub release downloads, used by tests/installer/run.sh.

Usage: server.py ROOT PORT_FILE

Listens on 127.0.0.1 only, on a free port that it writes to PORT_FILE once it
is ready. Request paths are /<mode>/<repo>/..., where <repo> is a directory
under ROOT laid out like a GitHub releases URL:

    <repo>/LATEST                    tag that <repo>/latest redirects to (optional)
    <repo>/download/<tag>/<asset>    release assets

and <mode> selects the behaviour:

    ok        like GitHub: latest -> 302 to tag/<tag> -> 200; download/... -> file or 404
    404, 500  every request gets that HTTP status
    drop      the connection is closed without any response
    truncate  like ok, but .tar.gz bodies stop halfway through
    slow      like ok, but first create ROOT/slow.started and wait 2 seconds
"""

import http.server
import os
import socket
import sys
import threading
import time

ROOT = ""


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        parts = self.path.split("?", 1)[0].split("/", 3)
        if len(parts) < 4 or parts[0] != "":
            return self.reply(404)
        _, mode, repo, rest = parts
        if mode in ("404", "500"):
            return self.reply(int(mode))
        if mode == "drop":
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
            return None
        if mode == "slow":
            open(os.path.join(ROOT, "slow.started"), "w").close()
            time.sleep(2)
        elif mode not in ("ok", "truncate"):
            return self.reply(404)
        repo_dir = self.safe_join(ROOT, repo)
        if repo_dir is None or not os.path.isdir(repo_dir):
            return self.reply(404)
        if rest == "latest":
            return self.redirect_latest(mode, repo, repo_dir)
        if rest.startswith("tag/"):
            return self.reply(200, b"<html>release page</html>\n")
        if rest.startswith("download/"):
            return self.send_asset(repo_dir, rest, truncate=(mode == "truncate" and rest.endswith(".tar.gz")))
        return self.reply(404)

    def redirect_latest(self, mode, repo, repo_dir):
        try:
            with open(os.path.join(repo_dir, "LATEST")) as f:
                tag = f.read().strip()
        except FileNotFoundError:
            return self.reply(404)
        host, port = self.server.server_address[:2]
        self.send_response(302)
        self.send_header("Location", f"http://{host}:{port}/{mode}/{repo}/tag/{tag}")
        self.send_header("Content-Length", "0")
        self.end_headers()
        return None

    def send_asset(self, repo_dir, rest, truncate):
        path = self.safe_join(repo_dir, rest)
        if path is None or not os.path.isfile(path):
            return self.reply(404)
        with open(path, "rb") as f:
            body = f.read()
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if truncate:
            self.wfile.write(body[: len(body) // 2])
            self.wfile.flush()
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
        else:
            self.wfile.write(body)
        return None

    @staticmethod
    def safe_join(base, rel):
        parts = rel.split("/")
        if any(p in ("", ".", "..") for p in parts):
            return None
        return os.path.join(base, *parts)

    def reply(self, status, body=b""):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("%s\n" % (fmt % args))
        sys.stderr.flush()


def exit_with_parent():
    """Stop serving if the test runner that started us goes away."""
    parent = os.getppid()
    while os.getppid() == parent:
        time.sleep(1)
    os._exit(0)


def main():
    global ROOT
    ROOT, port_file = sys.argv[1], sys.argv[2]
    threading.Thread(target=exit_with_parent, daemon=True).start()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    tmp = port_file + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(server.server_address[1]))
    os.rename(tmp, port_file)
    server.serve_forever()


if __name__ == "__main__":
    main()
