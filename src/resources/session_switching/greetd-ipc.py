#!/usr/bin/env python3
"""greetd IPC helper — create and start a user session via GREETD_SOCK."""

import json, os, socket, struct, sys

def _send(sock, msg):
    data = json.dumps(msg).encode()
    sock.sendall(struct.pack("=I", len(data)) + data)

def _recv(sock):
    raw = b""
    while len(raw) < 4:
        chunk = sock.recv(4 - len(raw))
        if not chunk:
            raise RuntimeError("greetd socket closed")
        raw += chunk
    length = struct.unpack("=I", raw)[0]
    data = b""
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise RuntimeError("greetd socket closed")
        data += chunk
    return json.loads(data)

def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} USERNAME CMD [ARGS...]", file=sys.stderr)
        return 1

    username, cmd = sys.argv[1], sys.argv[2:]
    sock_path = os.environ.get("GREETD_SOCK")
    if not sock_path:
        print("GREETD_SOCK not set", file=sys.stderr)
        return 1

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(sock_path)

    # create_session
    _send(sock, {"type": "create_session", "username": username})
    resp = _recv(sock)

    # handle auth challenge(s)
    while resp.get("type") == "auth_message":
        if resp.get("auth_message_type") in ("info", "error"):
            _send(sock, {"type": "post_auth_message_response"})
        else:
            _send(sock, {"type": "post_auth_message_response", "response": ""})
        resp = _recv(sock)

    if resp.get("type") != "success":
        desc = resp.get("description", resp.get("type", "unknown"))
        print(f"greetd auth failed: {desc}", file=sys.stderr)
        sock.close()
        return 1

    # start_session
    _send(sock, {"type": "start_session", "cmd": cmd})
    resp = _recv(sock)

    if resp.get("type") != "success":
        desc = resp.get("description", resp.get("type", "unknown"))
        print(f"greetd start_session failed: {desc}", file=sys.stderr)
        sock.close()
        return 1

    sock.close()
    return 0

if __name__ == "__main__":
    sys.exit(main())
