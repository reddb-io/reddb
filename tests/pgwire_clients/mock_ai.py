#!/usr/bin/env python3
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import argparse
import json
import os


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        if length:
            self.rfile.read(length)
        if self.path.endswith("/embeddings"):
            body = {
                "model": "mock-embedding",
                "data": [{"index": 0, "embedding": [1.0, 0.0, 0.0]}],
                "usage": {"prompt_tokens": 1, "total_tokens": 1},
            }
        else:
            prompt_tokens = int(os.environ.get("MOCK_AI_PROMPT_TOKENS", "1"))
            completion_tokens = int(os.environ.get("MOCK_AI_COMPLETION_TOKENS", "1"))
            body = {
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": "mock-chat",
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": "mock response"},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": prompt_tokens + completion_tokens,
                },
            }
        raw = json.dumps(body, separators=(",", ":")).encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def log_message(self, fmt, *args):
        return


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", required=True)
    args = parser.parse_args()
    host, port = args.listen.rsplit(":", 1)
    ThreadingHTTPServer((host, int(port)), Handler).serve_forever()


if __name__ == "__main__":
    main()
