#!/usr/bin/env python3
"""A canned OpenAI-compatible chat server for the app's headless LLM check.

Serves POST /chat/completions with one fixed rewritten score, so the
emulator check in android/README.md can point the app at
http://10.0.2.2:<port> (the emulator's name for the host's loopback) and
watch the Ask flow end with a score whose origin is unambiguous: the
reply carries the `rewritten by the mock llm` marker, which the engine's
diagnostics then echo after the app evaluates the rewritten score.

Usage: llm-mock-server.py [port]   (default port 8081)

Stdlib only; every request gets the same canned 200 OK reply.
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

REPLY_SCORE = (
    "console.log('rewritten by the mock llm');\n"
    '$: note("c4 e4 g4 b4 c5").s("sine").gain(0.2)\n'
    '$: s("bd sd hh cp").gain(0.5)\n'
)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        # Read the body before answering, or the connection breaks off
        # mid-send and the client sees a reset instead of a reply.
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        if self.path.split("?")[0] != "/chat/completions":
            self.send_error(404)
            return
        body = json.dumps({
            "choices": [{
                "message": {"role": "assistant", "content": REPLY_SCORE},
            }],
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("mock-llm: " + (fmt % args) + "\n")


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8081
    server = HTTPServer(("127.0.0.1", port), Handler)
    print(f"mock-llm: serving one canned rewrite on 127.0.0.1:{port}",
          file=sys.stderr)
    server.serve_forever()


if __name__ == "__main__":
    main()
