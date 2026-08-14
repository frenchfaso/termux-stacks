#!/usr/bin/env python3
"""Serve one OCI archive over loopback with a deterministic download hold."""

from __future__ import annotations

import argparse
import http.server
import os
from pathlib import Path
import sys
import time


class FixtureServer(http.server.HTTPServer):
    archive: Path
    events: Path
    release: Path
    chunk_size: int
    deadline: float
    request_complete: bool

    def record(self, event: str, value: str = "-") -> None:
        line = f"{time.time_ns()}\t{event}\t{value}\n".encode()
        fd = os.open(self.events, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        try:
            os.write(fd, line)
            os.fsync(fd)
        finally:
            os.close(fd)


class FixtureHandler(http.server.BaseHTTPRequestHandler):
    server: FixtureServer

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def _headers(self) -> bool:
        if self.path != "/fixture.oci.tar":
            self.send_error(404)
            self.server.record("rejected_path", self.path)
            return False
        size = self.server.archive.stat().st_size
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(size))
        self.send_header("Connection", "close")
        self.end_headers()
        return True

    def do_HEAD(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self._headers():
            self.server.record("head", self.path)

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if not self._headers():
            return

        sent = 0
        disconnected = False
        completed = False
        self.server.record("get", self.path)
        try:
            with self.server.archive.open("rb") as archive:
                first = archive.read(self.server.chunk_size)
                if not first:
                    raise RuntimeError("fixture archive is empty")
                self.wfile.write(first)
                self.wfile.flush()
                sent += len(first)
                self.server.record("barrier", str(sent))

                while (
                    not self.server.release.exists()
                    and time.monotonic() < self.server.deadline
                ):
                    time.sleep(0.02)
                if self.server.release.exists():
                    self.server.record("released", str(sent))
                else:
                    self.server.record("server_timeout", str(sent))
                    return

                while True:
                    chunk = archive.read(self.server.chunk_size)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    sent += len(chunk)
                self.wfile.flush()
                completed = True
        except (BrokenPipeError, ConnectionResetError):
            disconnected = True
            self.server.record("client_disconnected", str(sent))
        except OSError as error:
            disconnected = True
            self.server.record("io_error", type(error).__name__)
        finally:
            if completed and not disconnected:
                self.server.record("complete", str(sent))
            self.server.request_complete = True


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--ready", required=True, type=Path)
    parser.add_argument("--events", required=True, type=Path)
    parser.add_argument("--release", required=True, type=Path)
    parser.add_argument("--chunk-size", type=int, default=4096)
    parser.add_argument("--ttl-seconds", type=float, default=90.0)
    return parser.parse_args()


def validate_private_path(path: Path, label: str, must_exist: bool = False) -> Path:
    if not path.is_absolute():
        raise ValueError(f"{label} must be absolute")
    parent = path.parent.resolve(strict=True)
    if parent.is_symlink() or not parent.is_dir():
        raise ValueError(f"{label} parent must be a real directory")
    resolved = parent / path.name
    if must_exist and (not resolved.is_file() or resolved.is_symlink()):
        raise ValueError(f"{label} must be a regular non-symlink file")
    if not must_exist and (resolved.exists() or resolved.is_symlink()):
        raise ValueError(f"{label} must not already exist")
    return resolved


def write_ready(path: Path, port: int) -> None:
    temporary = path.parent / f".{path.name}.{os.getpid()}.tmp"
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        os.write(fd, f"v1\t{port}\n".encode())
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(temporary, path)
    directory_fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def main() -> int:
    args = parse_args()
    if args.chunk_size < 1 or args.chunk_size > 1024 * 1024:
        raise ValueError("chunk size must be between 1 and 1048576")
    if args.ttl_seconds <= 0:
        raise ValueError("TTL must be positive")

    archive = validate_private_path(args.archive, "archive", must_exist=True)
    ready = validate_private_path(args.ready, "ready file")
    events = validate_private_path(args.events, "events file")
    release = validate_private_path(args.release, "release file")

    server = FixtureServer(("127.0.0.1", 0), FixtureHandler)
    server.archive = archive
    server.events = events
    server.release = release
    server.chunk_size = args.chunk_size
    server.deadline = time.monotonic() + args.ttl_seconds
    server.request_complete = False
    server.timeout = 0.5
    server.record("listening", str(server.server_port))
    write_ready(ready, server.server_port)

    while not server.request_complete and time.monotonic() < server.deadline:
        server.handle_request()
    if not server.request_complete:
        server.record("server_timeout")
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError) as error:
        print(f"slow-http-s4: {error}", file=sys.stderr)
        raise SystemExit(1) from error
