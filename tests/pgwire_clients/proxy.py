#!/usr/bin/env python3
import argparse
import json
import re
import socket
import socketserver
import struct
import threading


def read_exact(sock, n):
    chunks = []
    remaining = n
    while remaining:
        data = sock.recv(remaining)
        if not data:
            return None
        chunks.append(data)
        remaining -= len(data)
    return b"".join(chunks)


def parse_cstrings(payload):
    parts = payload.split(b"\x00")
    out = {}
    for idx in range(0, max(0, len(parts) - 1), 2):
        key = parts[idx]
        value = parts[idx + 1]
        if not key:
            break
        out[key.decode("utf-8", "replace")] = value.decode("utf-8", "replace")
    return out


def parse_cstring(payload, offset=0):
    end = payload.find(b"\x00", offset)
    if end < 0:
        return ""
    return payload[offset:end].decode("utf-8", "replace")


class Proxy(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True

    def __init__(self, listen, target, log_path):
        super().__init__(listen, Handler)
        self.target = target
        self.log_path = log_path
        self.lock = threading.Lock()

    def log(self, event):
        with self.lock:
            with open(self.log_path, "a", encoding="utf-8") as f:
                f.write(json.dumps(event, sort_keys=True) + "\n")


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        upstream = socket.create_connection(self.server.target)
        done = threading.Event()
        downstream = threading.Thread(
            target=self.pipe_server_to_client, args=(upstream, done), daemon=True
        )
        downstream.start()
        try:
            self.pipe_client_to_server(upstream, done)
        finally:
            done.set()
            for sock in (self.request, upstream):
                try:
                    sock.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                sock.close()

    def pipe_server_to_client(self, upstream, done):
        while not done.is_set():
            data = upstream.recv(65536)
            if not data:
                done.set()
                return
            self.request.sendall(data)

    def pipe_client_to_server(self, upstream, done):
        startup_done = False
        app = "unknown"
        while not done.is_set():
            if not startup_done:
                header = read_exact(self.request, 4)
                if header is None:
                    return
                length = struct.unpack("!I", header)[0]
                payload = read_exact(self.request, length - 4)
                if payload is None:
                    return
                upstream.sendall(header + payload)
                if length >= 8:
                    code = struct.unpack("!I", payload[:4])[0]
                    if code == 196608:
                        params = parse_cstrings(payload[4:])
                        app = params.get("application_name", app)
                        startup_done = True
                        self.server.log({"app": app, "tag": "startup", "params": params})
                continue

            tag = read_exact(self.request, 1)
            if tag is None:
                return
            header = read_exact(self.request, 4)
            if header is None:
                return
            length = struct.unpack("!I", header)[0]
            payload = read_exact(self.request, length - 4)
            if payload is None:
                return
            upstream.sendall(tag + header + payload)

            tag_text = tag.decode("ascii", "replace")
            event = {"app": app, "tag": tag_text}
            if tag == b"Q":
                query = parse_cstring(payload)
                event["query"] = query
                match = re.match(r"\s*set\s+application_name\s*=\s*'([^']+)'", query, re.I)
                if match:
                    app = match.group(1)
                    event["app"] = app
            elif tag == b"P":
                statement = parse_cstring(payload)
                query = parse_cstring(payload, len(statement.encode("utf-8")) + 1)
                event["statement"] = statement
                event["query"] = query
            self.server.log(event)


def parse_addr(value):
    host, port = value.rsplit(":", 1)
    return host, int(port)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--log", required=True)
    args = parser.parse_args()
    with Proxy(parse_addr(args.listen), parse_addr(args.target), args.log) as proxy:
        proxy.serve_forever()


if __name__ == "__main__":
    main()
