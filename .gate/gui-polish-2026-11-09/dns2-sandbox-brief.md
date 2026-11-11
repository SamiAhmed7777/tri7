# Judge brief: DNS2 tri7d sandbox ReadWritePaths fix (GUI-POLISH tick companion)

## What
One systemd drop-in on DNS2: /etc/systemd/system/tri7d.service.d/sandbox-rw.conf

[Service]
ReadWritePaths=/var/lib/tri7 /etc/tri7

## Why (measured, live)
The tri7d unit ships ProtectSystem=strict with ReadWritePaths=/var/lib/tri7
only. The new `reservebalance` verb (and the pre-existing `staking` verb)
persists its setting into /etc/tri7/triangles.conf by the same conf_set_flag
write path. On DNS2 every such write fails live:

  {"error":{"code":-1,"message":"write /etc/tri7/triangles.conf.tmp:
   Read-only file system (os error 30)"}}

(raw RPC transcript captured 2026-11-09 ~18:5xZ on 127.0.0.1:19112 after the
item-15 binary deploy; the live in-memory global DID apply — the persist is
what failed.) The identical defect was found and fixed on DNS3 during item
11 with a supplementary judge grade B
(urn:ump:o4rmdvknvaf5c26qbdgcn3sls4n5du7g5edtncvfezonhkbj34pa); DNS2's unit
was never given the same fix until the reserve verb exposed it again.

## Correctness notes
- Drop-in ReadWritePaths= REPLACES the main unit's list (systemd semantics),
  so both paths are re-listed (/var/lib/tri7 for the chain/wallet state,
  /etc/tri7 for the conf). Nothing else is widened.
- conf perms are already correct: -rw-rw---- root:tri7 (the daemon's User=tri7
  has group-write); only the mount namespace blocked it.
- The conf's stake=1 line is the boot default for DNS2 staking (the sole
  heavy staker stays staking=on across restarts) — this fix makes future
  staking on/off + reservebalance persists actually land, which previously
  failed silently-in-effect on DNS2 (the verb returned -1 but operators
  treated staking as conf-managed).

## Test evidence (executed on DNS2 after applying + daemon-reload + restart)
Planned and to be attached post-apply: SET 1.0 → conf line appears → SET
0.000000 → conf line updates → getstakinginfo reflects the value; staking
enabled=true throughout; tip advancing. (Applied via systemctl daemon-reload
+ systemctl restart tri7d; restart is the standard warm cycle, ~30-90s.)

## Answer shape
A single JSON object: {"codex_grade": "A"|"B"|"C"|"D"|"F", "reasons": [...],
"blocking_issues": [...], "polish_suggestions": [...]}. Last line, nothing
after.
