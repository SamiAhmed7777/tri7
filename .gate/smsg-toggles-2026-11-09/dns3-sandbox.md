# Supplementary delta: DNS3 tri7d unit sandbox fix (ReadWritePaths += /etc/tri7)

## The defect (found live during the item-11 SMSG-TOGGLES deploy)
DNS3's `tri7d.service` runs with `ProtectSystem=strict` and `ReadWritePaths=/var/lib/tri7`.
Consequence: the daemon's `staking "on"` RPC engages staking in-RAM but can NEVER persist
`staking=1` to `/etc/tri7/triangles.conf` — the write fails with EROFS ("Read-only file
system (os error 30)") because systemd bind-mounts the whole FS read-only for the unit.
This is the real root cause of the item-10 workqueue caveat "conf flip blocked … in-RAM
only; re-assert after any DNS3 restart" (mis-diagnosed as cron approvals). Every DNS3
restart silently drops staking.

Note: `chmod 770 /etc/tri7` was applied first (group tri7 lacked w) — necessary but not
sufficient; the EROFS persisted because the restriction is the unit sandbox, not file
permissions (proof: `sudo -u tri7 touch /etc/tri7/.wtest3` succeeds as the same uid the
daemon runs as, while the daemon's create still returns EROFS).

## The fix (exact bytes)
`/etc/systemd/system/tri7d.service.d/sandbox.conf` (new drop-in on DNS3 only):
```
[Service]
# staking "on" must be able to persist staking=1 to /etc/tri7/triangles.conf —
# ProtectSystem=strict made /etc read-only for the unit (EROFS on the conf write),
# so every restart silently dropped staking (workqueue item-10 caveat, root-caused
# 2026-11-09 during the item-11 deploy)
ReadWritePaths=/var/lib/tri7 /etc/tri7
```
Then `systemctl daemon-reload && systemctl restart tri7d`.

## Blast radius
- One line added to one drop-in on ONE host (DNS3). DNS2 keeps its own unit untouched
  (its conf persists fine today). The wallet/store/RPC surface stays scoped to
  /var/lib/tri7 (unchanged); /etc/tri7 gains unit-write access for the conf file the
  daemon already owns group-write on (tri7:tri7 0664 after this tick's chmod 770).
- Verification performed after applying (see the tick transcript): `staking "on"` writes
  `staking=1` into /etc/tri7/triangles.conf, getstakinginfo enabled=true survives, and
  the restart caveat is retired.

Answer with a single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": ["..."], "blocking_issues": ["..."], "polish_suggestions": ["..."]}
