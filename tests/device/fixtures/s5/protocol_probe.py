#!/data/data/com.termux/files/usr/bin/python3
"""Exercise exact replay and version-mismatch behavior over the S5 socket."""

from __future__ import annotations

import json
from pathlib import Path
import socket
import sys

MAX_FRAME = 1024 * 1024


def fail(message: str) -> "NoReturn":
    print(f"protocol-probe: {message}", file=sys.stderr)
    raise SystemExit(1)


def exchange(socket_path: Path, request: dict[str, object]) -> bytes:
    frame = json.dumps(request, separators=(",", ":"), ensure_ascii=False).encode() + b"\n"
    if len(frame) > MAX_FRAME:
        fail("request exceeds protocol frame limit")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(15)
        stream.connect(str(socket_path))
        stream.sendall(frame)
        received = bytearray()
        while not received.endswith(b"\n"):
            chunk = stream.recv(65536)
            if not chunk:
                fail("daemon closed the socket before the newline-terminated response")
            received.extend(chunk)
            if len(received) > MAX_FRAME + 1:
                fail("response exceeds protocol frame limit")
    return bytes(received)


def decode(frame: bytes) -> dict[str, object]:
    try:
        value = json.loads(frame)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"response is not valid JSON: {error}")
    if not isinstance(value, dict):
        fail("response is not a JSON object")
    return value


def main() -> None:
    if len(sys.argv) != 4:
        print("Usage: protocol_probe.py SOCKET MANIFEST OUTPUT_DIRECTORY", file=sys.stderr)
        raise SystemExit(2)
    socket_path = Path(sys.argv[1])
    manifest_path = Path(sys.argv[2])
    output_directory = Path(sys.argv[3])
    if not socket_path.is_absolute() or not manifest_path.is_file() or not output_directory.is_dir():
        fail("socket, manifest, or output directory is invalid")

    manifest = manifest_path.read_text(encoding="utf-8")
    up_request: dict[str, object] = {
        "command": "up",
        "protocol_version": 1,
        "request_id": "s5-replay-1",
        "manifest": manifest,
    }
    first = exchange(socket_path, up_request)
    second = exchange(socket_path, up_request)
    (output_directory / "protocol-replay-first.jsonl").write_bytes(first)
    (output_directory / "protocol-replay-second.jsonl").write_bytes(second)
    if first != second:
        fail("replayed response bytes differ")
    first_value = decode(first)
    if first_value.get("ok") is not True or first_value.get("request_id") != "s5-replay-1":
        fail("replayed up response is not successful")

    mismatch_request: dict[str, object] = {
        "command": "status",
        "protocol_version": 2,
        "request_id": "s5-version-mismatch-1",
        "stack": "s5-normal",
    }
    mismatch = exchange(socket_path, mismatch_request)
    (output_directory / "protocol-version-mismatch.jsonl").write_bytes(mismatch)
    mismatch_value = decode(mismatch)
    error = mismatch_value.get("error")
    if (
        mismatch_value.get("protocol_version") != 1
        or mismatch_value.get("request_id") != "s5-version-mismatch-1"
        or mismatch_value.get("ok") is not False
        or not isinstance(error, dict)
        or error.get("code") != "protocol_error"
        or "unsupported protocol version 2; expected 1" not in str(error.get("message"))
    ):
        fail("version mismatch did not fail closed with the v1 response envelope")

    print("replay=identical")
    print("version_mismatch=protocol_error")


if __name__ == "__main__":
    main()
