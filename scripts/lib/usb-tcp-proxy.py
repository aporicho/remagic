#!/usr/bin/env python3
"""Connect a stdio stream to a TCP endpoint through one USB interface."""

from __future__ import annotations

import os
import re
import select
import socket
import sys


SO_BINDTODEVICE = 25


def write_all(fd: int, data: bytes) -> None:
    view = memoryview(data)
    while view:
        written = os.write(fd, view)
        view = view[written:]


def main() -> int:
    if len(sys.argv) != 4:
        print("usage: usb-tcp-proxy.py INTERFACE HOST PORT", file=sys.stderr)
        return 2

    interface, host, port_text = sys.argv[1:]
    if not re.fullmatch(r"[A-Za-z0-9_.:-]+", interface):
        print(f"invalid USB interface name: {interface}", file=sys.stderr)
        return 2
    if not os.path.isdir(f"/sys/class/net/{interface}"):
        print(f"USB interface does not exist: {interface}", file=sys.stderr)
        return 2

    try:
        port = int(port_text)
    except ValueError:
        print(f"invalid TCP port: {port_text}", file=sys.stderr)
        return 2


    connection = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    connection.setsockopt(
        socket.SOL_SOCKET,
        SO_BINDTODEVICE,
        interface.encode("utf-8") + b"\0",
    )
    connection.connect((host, port))

    input_open = True
    while True:
        readers = [connection]
        if input_open:
            readers.append(sys.stdin.buffer)
        ready, _, _ = select.select(readers, [], [])

        if connection in ready:
            data = connection.recv(65536)
            if not data:
                return 0
            write_all(sys.stdout.fileno(), data)

        if input_open and sys.stdin.buffer in ready:
            data = os.read(sys.stdin.fileno(), 65536)
            if data:
                connection.sendall(data)
            else:
                input_open = False
                connection.shutdown(socket.SHUT_WR)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (BrokenPipeError, ConnectionError, OSError) as error:
        print(f"USB proxy: {error}", file=sys.stderr)
        raise SystemExit(1) from error
