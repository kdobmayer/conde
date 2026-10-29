# CI only (ci.yml windows-install): `python signin-front.py <listen port> <server port> <host key> <user>`.
# The workspace's sign-in in front of a real `conde server`: /auth/cli (code mode: a page that
# shows the code) and /auth/cli/redeem answer here; every other connection is passed through
# byte for byte, the bridge's WebSocket included.
import base64, hashlib, json, secrets, socket, sys, threading
from urllib.parse import urlsplit, parse_qs

LISTEN, UPSTREAM, KEY, USER = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4]
waiting = {}  # state -> (code, challenge)

def reply(conn, status, body, ctype="application/json"):
    data = body.encode()
    conn.sendall(f"HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {len(data)}\r\nConnection: close\r\n\r\n".encode() + data)

def pipe(a, b):
    try:
        while True:
            d = a.recv(65536)
            if not d:
                break
            b.sendall(d)
    except OSError:
        pass
    finally:
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

def handle(conn):
    buf = b""
    while b"\r\n\r\n" not in buf:
        d = conn.recv(65536)
        if not d:
            conn.close()
            return
        buf += d
    head, _, rest = buf.partition(b"\r\n\r\n")
    method, target = head.split(b"\r\n")[0].decode().split(" ")[:2]
    path = urlsplit(target)
    if path.path == "/auth/cli":
        q = {k: v[0] for k, v in parse_qs(path.query).items()}
        code = "".join(secrets.choice("23456789ABCDEFGHJKMNPQRSTVWXYZ") for _ in range(8))
        waiting[q["state"]] = (code, q["challenge"])
        reply(conn, "200 OK", f'<pre id="key">{code[:4]}-{code[4:]}</pre>', "text/html")
        conn.close()
        return
    if path.path == "/auth/cli/redeem":
        length = int([l.split(b":")[1] for l in head.split(b"\r\n") if l.lower().startswith(b"content-length:")][0])
        while len(rest) < length:
            rest += conn.recv(65536)
        req = json.loads(rest[:length])
        code, challenge = waiting.get(req.get("state"), (None, None))
        sent = req.get("code", "").replace("-", "").upper()
        verifier = req.get("code_verifier", "").encode()
        derived = base64.urlsafe_b64encode(hashlib.sha256(verifier).digest()).rstrip(b"=").decode()
        if code is None or sent != code or derived != challenge:
            reply(conn, "400 Bad Request", json.dumps({"error": "invalid_grant", "message": "wrong code"}))
        else:
            reply(conn, "200 OK", json.dumps({"host_id": req["host"], "key": KEY, "user": USER, "reenrolled": False}))
        conn.close()
        return
    up = socket.create_connection(("127.0.0.1", UPSTREAM))
    up.sendall(buf)
    threading.Thread(target=pipe, args=(up, conn), daemon=True).start()
    pipe(conn, up)

srv = socket.create_server(("127.0.0.1", LISTEN))
while True:
    c, _ = srv.accept()
    threading.Thread(target=handle, args=(c,), daemon=True).start()
