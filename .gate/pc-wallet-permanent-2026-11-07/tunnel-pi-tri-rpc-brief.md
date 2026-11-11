# Expose the Raspberry Pi's Triangles daemon RPC to DNS2 over the tailnet

WHY: the Pi's daemon runs with `--rpc-port 19112 --rpc-bind 127.0.0.1`. The
daemon ENFORCES loopback-only binds ("--rpc-bind ADDR loopback-only bind,
default 127.0.0.1 (non-loopback addresses are refused)") and does no auth on
loopback by design. So the Pi's RPC is up and answering, but only reachable
from the Pi itself - which is why it looked "missing".

The supported way to reach it from another host is an SSH tunnel - exactly the
pattern this box already runs for the PC:
  /etc/systemd/system/tunnel-pc-tri-rpc.service
    DNS2:19113 -> PC:19112 over the tailnet (100.85.236.10)

THIS CHANGE adds the same for the Pi:
  /etc/systemd/system/tunnel-pi-tri-rpc.service
    DNS2:19115 -> Pi:19112 over the tailnet (100.106.76.60)

Facts that were verified before writing this:
- `ssh root@100.106.76.60` works and the host key for 100.106.76.60 is already
  in /root/.ssh/known_hosts (line 43), so StrictHostKeyChecking=yes is used
  (the older PC unit uses "no"; "yes" is stricter and correct here).
- The Pi's RPC answers on its loopback: getblockcount -> 2207591.
- The Pi's daemon keeps its RPC unauthenticated on loopback (documented
  behaviour: "no auth on loopback (same trust boundary)"). The tunnel's local
  end is bound to 127.0.0.1 only (autossh -L binds loopback by default), so the
  exposure is identical to the existing PC tunnel - nothing new is published to
  the public internet or the tailnet.
- Port 19115 is free on DNS2 (19112 = local daemon, 19113 = PC tunnel).

Operations to perform after this is approved:
  install the unit, `systemctl daemon-reload`,
  `systemctl enable --now tunnel-pi-tri-rpc.service`,
  then verify by calling the Pi's RPC through the tunnel:
    curl -s -X POST http://127.0.0.1:19115 -d '{"method":"getblockcount","params":[]}'
    curl -s -X POST http://127.0.0.1:19115 -d '{"method":"getwalletinfo","params":[]}'
  and confirm the numbers match the Pi's own loopback answers.

No production code changes; no change to any wallet file; no change to the Pi's
own configuration.
