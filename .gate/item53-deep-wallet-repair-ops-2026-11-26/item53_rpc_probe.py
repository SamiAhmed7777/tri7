#!/usr/bin/env python3
"""Print the node's block count (RPC getblockcount), or exit 1 without a word.

The RPC credential is read from the node's own configuration file and sent
in the request header only: it is never printed, logged or put on a command
line.
"""
import base64
import http.client
import json
import re
import sys

try:
    conf = open("/etc/tri7/triangles.conf").read()

    def key(name):
        m = re.search(r"^" + name + r"=(\S+)", conf, re.M)
        return m.group(1) if m else None

    user, password = key("rpcuser"), key("rpcpassword")
    port = int(key("rpcport") or 19112)
    if not user or not password:
        raise ValueError
    auth = base64.b64encode((user + ":" + password).encode()).decode()
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    body = json.dumps({"jsonrpc": "1.0", "id": 1, "method": "getblockcount", "params": []})
    conn.request("POST", "/", body, {"Authorization": "Basic " + auth,
                                     "Content-Type": "application/json"})
    reply = json.loads(conn.getresponse().read())
    if reply.get("error") or "result" not in reply:
        raise ValueError
    print(reply["result"])
except Exception:
    # no detail on purpose: an exception text could echo the request
    sys.exit(1)
