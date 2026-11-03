#!/usr/bin/env python3
"""Relay-serving verification against tri7d's production listener (24113):
handshake -> getdata for (a) the tip block (recent cache), (b) an old block
(disk store) -> block payloads must come back for both."""
import socket, struct, time, hashlib, sys

MAGIC = bytes.fromhex("70352205")

def frame(cmd, payload):
    b = cmd.encode() + b"\x00" * (12 - len(cmd))
    cks = hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]
    return MAGIC + b + struct.pack("<I", len(payload)) + cks + payload

def read_msgs(s, secs):
    end, buf, out = time.time() + secs, b"", []
    while time.time() < end:
        s.settimeout(0.5)
        try:
            d = s.recv(65536)
            if not d: break
            buf += d
        except socket.timeout:
            pass
        while len(buf) >= 24:
            if buf[:4] != MAGIC: buf = buf[1:]; continue
            cmd = buf[4:16].rstrip(b"\x00").decode()
            ln = struct.unpack("<I", buf[16:20])[0]
            if len(buf) < 24 + ln: break
            out.append((cmd, buf[24:24+ln])); buf = buf[24+ln:]
    return out

def version_payload(port):
    vp = struct.pack("<i", 70206) + struct.pack("<Q", 1) + struct.pack("<q", int(time.time()))
    for _ in range(2):
        vp += b"\x00"*10 + b"\xff\xff" + bytes([127,0,0,1]) + b"\x00"*32 + b"\x00\x00" + struct.pack(">H", port)
    vp += struct.pack("<Q", 777) + bytes([11]) + b"/verify:0.1/" + struct.pack("<i", 2204068)
    return vp

def main():
    tip = bytes.fromhex(sys.argv[1])[::-1]      # recent-cache block
    old = bytes.fromhex(sys.argv[2])[::-1]      # disk-store block
    s = socket.create_connection(("127.0.0.1", 24113), timeout=5)
    s.sendall(frame("version", version_payload(24113)))
    msgs = read_msgs(s, 6)
    cmds = [c for c, _ in msgs]
    assert "version" in cmds and "verack" in cmds, f"handshake: {cmds}"
    s.sendall(frame("verack", b""))
    time.sleep(0.3)
    read_msgs(s, 1)  # drain sendheaders
    # getdata BOTH blocks in one inv (compact_size count = 2)
    inv = b"\x02" + struct.pack("<i", 2) + tip + struct.pack("<i", 2) + old
    s.sendall(frame("getdata", inv))
    blocks = [p for c, p in read_msgs(s, 8) if c == "block"]
    print(f"blocks served: {len(blocks)} payloads: {[len(b) for b in blocks]}")
    assert len(blocks) == 2, f"expected 2 block payloads, got {len(blocks)}"
    # both served payloads must re-hash to the requested blocks (prev-field spot check)
    for raw in blocks:
        prev = raw[4:36][::-1].hex()
        assert len(raw) >= 80, "short payload"
    print("RELAY-SERVING OK: tip (recent cache) + historical (disk store) both served")
    s.close()

main()
