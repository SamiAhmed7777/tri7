# Brief: tailscale-route-resilience skill — add 3 diagnostic pitfalls

Purpose: fold lessons from a live incident (2026-11-18, DNS2↔Pi management
plane outage during the tri7 continuity sweep) into the existing
tailscale-route-resilience skill so future triage skips the wrong branches.

What happened (verified live):
- DNS2→Pi (Hetzner HEL1) went ~90% packet loss, size-independent: a 1472B
  DF ping passed while 1400B died — NOT an MTU/PMTUD blackhole.
- Plain ssh over the WAN TCP-connected but died in banner exchange (random
  loss kills streams, not SYNs).
- The peer's tailscaled (on Pi) answered a THIRD node (DNS3) instantly —
  netstack SYN answers don't prove the host's userspace is healthy end-to-end,
  but DO separate "route bad" from "host down" when the third node's path is clean.
- Relay attempts (ssh -J / ProxyCommand via dns3) were DENIED by Pi's
  Tailscale-SSH layer: "tailnet policy does not permit you to SSH to this
  node" — policy keys on the TAILNET SOURCE of the connection (the relay
  peer), not the authenticating client. nc -z to :22 proves only the
  network-ACL layer, not the sshserver policy layer.
- DNS2's tailscaled logged DERP region-28 (hel) dial timeouts — the affected
  peers' home region — while DNS2's own nearest DERP (sin) was healthy.

Change: ADD three pitfalls to the existing Pitfalls section of
skills/devops/tailscale-route-resilience/SKILL.md (pure addition, no
existing text modified, no commands changed).

Review focus: (1) technical correctness of the three statements; (2) no
scope creep beyond the pitfalls list; (3) phrasing consistent with the
skill's imperative style.
