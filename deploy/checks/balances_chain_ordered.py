#!/usr/bin/env python3
"""Acceptance check for the first chain-ordered balances build of a tri7d
process (tri7 item 48 BALANCES-REPLAY-LOSER).

    balances_chain_ordered.py <restart epoch> [--unit tri7d]
                                        [--snapshot /var/lib/tri7/balances.json]
    balances_chain_ordered.py --selftest

Reads the journal of the unit's CURRENT main process, so lines written by the
process that was stopped cannot be mistaken for the new one, and the snapshot
file that process writes.

Exit 0  accepted. A build this process finished replayed in index order,
        covered at least the tip it chased (the chain may advance while the
        walk runs), and the snapshot file agrees with the journal line.
        The verdict is taken from the LAST build that finished: a build that
        landed short of the tip is superseded by its own retry.
Exit 3  still pending. No build has finished (a failed build with the
        scheduler's retry still to come is pending, not a rejection — the
        harness's --wait-secs deadline is what turns a chain of failures
        into a rollback), or the last finished build landed short of the
        tip it chased and the node scheduled its retry — on a live node the
        block store's bodies lag the moving tip, and the retry continues
        from the NEW snapshot (live on the DNS3 canary 2026-11-25: done
        h2219460 vs chased h2219462, "retrying in 60s", and the retry's
        start line loaded snapshot h2219460; the canary was rolled back
        before that retry wrote its own done line, so the completed-retry
        shape is covered only by the labelled synthetic selftest case).
Exit 1  rejected. A finished build replayed in file order (the binary
        predates the fix), landed short of the tip it chased with NO retry
        scheduled, or the snapshot file disagrees with the journal line.
Exit 2  cannot check (bad arguments).

Only a node whose on-disk snapshot is stale runs a build after a restart. On
a node with a fresh snapshot nothing is built and this check stays pending.
"""
import json
import re
import subprocess
import sys

START = re.compile(r"balances-build: start \(snapshot h(\d+), chain tip h(\d+)\)")
DONE = re.compile(r"balances-build: done in [\d.]+s — coverage h(\d+) tip ([0-9a-f]{64})")
RETRY = re.compile(r"balances-build: coverage h(\d+) short of the chased tip h(\d+) — retrying in \d+s")


def order_of(line):
    """Replay order a `done` line reports. Lines written before item 48 carry
    no order field; those builds replayed in file order."""
    if " · index order → " in line:
        return "index"
    if " · file order → " in line:
        return "file"
    return "file (pre-item-48 log line, no order field)"


def tail(line):
    """The message after the timestamp prefix, clipped."""
    return line[line.index("balances-build:"):][:200] if "balances-build:" in line else line[:200]


def judge(lines, snapshot):
    """(exit code, reason) for the journal lines of one tri7d process.

    The verdict is taken from the LAST build that FINISHED (a build that
    landed short of the tip it chased is superseded by its own retry, which
    starts from the new snapshot). A finished build in file order or with a
    disagreeing snapshot is a rejection — those outcomes are deterministic
    and no retry changes them. A build that finished short of the tip it
    chased is PENDING when the node scheduled its retry (its own journal
    line) or a newer build began: on a live node the block store's bodies
    lag the moving tip, and the retry continues from the new snapshot as
    the bodies catch up. No finished
    build yet is pending — failures and capped walks are retried, and the
    harness's deadline is the backstop that turns a non-converging chain
    into a rollback.
    """
    snapshot_h = chased = None
    covered = tip = order = None
    seen_fail = False
    retry_scheduled = False   # the node's own "coverage hC short of the chased tip hT — retrying" line, height-matched to the judged build
    for line in lines:
        if "balances-build:" not in line:
            continue
        m = START.search(line)
        if m:
            snapshot_h, chased = int(m.group(1)), int(m.group(2))
            if covered is not None:
                covered = tip = order = None  # a newer build is in flight; its own done line is the one to judge
            continue  # every start line resets the chased tip for its build
        if "balances-build: FAILED" in line:
            seen_fail = True
            continue
        if "balances-build: chain-ordered walk stopped at h" in line:
            continue  # the walk capped where the store's bodies end; the retry continues from the new snapshot
        m = RETRY.search(line)
        if m:
            # associate the retry with the finished build it answers: the node
            # prints the judged build's own coverage and chased heights in it.
            # A retry line with other heights belongs to a different build and
            # must not mark this one pending.
            if covered is not None and int(m.group(1)) == covered and int(m.group(2)) == chased:
                retry_scheduled = True
            continue
        m = DONE.search(line)
        if not m or covered is not None:
            continue  # progress lines, retries, or an already-superseded build
        if chased is None:
            return 1, "a build finished without a start line: coverage h%d tip %s…" % (int(m.group(1)), m.group(2)[:16])
        covered, tip, order = int(m.group(1)), m.group(2), order_of(line)
        retry_scheduled = False  # this finished build supersedes what preceded it
    if covered is None:
        if snapshot_h is None and not seen_fail:
            return 3, "no build has started yet (the daemon may still be loading the block index)"
        if seen_fail:
            return 3, "a build failed and the scheduler is retrying it: %s" % tail(next(l for l in reversed(lines) if "balances-build: FAILED" in l))
        return 3, "the build is running"
    assert tip is not None and order is not None and chased is not None
    if order != "index":
        return 1, "the build replayed in %s order — this binary predates the fix: coverage h%d tip %s…" % (order, covered, tip[:16])
    if covered < chased:
        if retry_scheduled:
            return 3, "the build landed %d block(s) short of the advancing tip (h%d of h%d) and the node scheduled its incremental retry — live tip, bodies lag the index" % (chased - covered, covered, chased)
        return 1, "coverage h%d is short of the chased tip h%d and no retry was scheduled for it" % (covered, chased)
    if snapshot is None:
        return 1, "the snapshot file cannot be read"
    # Production snapshot semantics (src/balances_build.rs): height = the last
    # block applied (covered_to), covered_tip_hash = its hash,
    # replay_target_height = the tip the build WAS CHASING (the start line's
    # second height). If the chain advanced mid-walk, covered exceeds it.
    on_disk = (snapshot.get("height"), snapshot.get("covered_tip_hash"), snapshot.get("replay_target_height"))
    if on_disk != (covered, tip, chased):
        return 1, "the snapshot file disagrees with the journal: height=%r tip=%r replay_target_height=%r vs covered h%d/chased h%d tip %s…" % (
            on_disk[0], str(on_disk[1])[:16], on_disk[2], covered, chased, tip[:16])
    return 0, "finished build of this process: index order, coverage h%d at or above the chased tip h%d, snapshot file agrees, tip %s…" % (
        covered, chased, tip[:16])


def selftest():
    tip = "ab" * 32
    tip2 = "cd" * 32
    good_done = ("[2026-11-25T06:00:00Z] balances-build: done in 90.0s — coverage h2219400 tip %s · supply 226992597204 sats · "
                 "18047 live UTXOs · wallet 10 keys 100000000 sats · index order → /var/lib/tri7/balances.json" % tip)
    # the shape every node printed before the fix (DNS3 journal, 2026-11-25T05:48:13Z)
    old_done = ("[2026-11-25T05:48:13Z] balances-build: done in 86.4s — coverage h2208246 tip 203294d2c94583434aad5299e862e8f10bfef685fc62b82ba175a747f1a878b8 · "
                "supply 226841559503 sats · 18047 live UTXOs · wallet 10 keys 100000000 sats → /var/lib/tri7/balances.json")
    start = "balances-build: start (snapshot h2208246, chain tip h2219400) — replaying /var/lib/tri7/blk0001.dat"
    agree = {"height": 2219400, "covered_tip_hash": tip, "replay_target_height": 2219400}
    advanced_done = good_done.replace("h2219400", "h2219450").replace(tip, tip2)
    fail_line = "balances-build: FAILED (attempt 1/12): replay error — retrying in 30s"
    # the live DNS3 canary, 2026-11-25 09:21-09:26Z, verbatim shapes: the first
    # index-ordered build finished 2 blocks short of the advancing tip (the
    # store's bodies lagged), the node scheduled its own retry, and the retry's
    # start line loaded the NEW snapshot. The canary rolled back before that
    # retry wrote a done line; the completed-retry shapes below (d3_done2) are
    # synthetic, labelled as such in their case names.
    d3_tip = "e4c2f6754aa2718d48da8b7f47b3de71d7a1aa3d47f7fcd4a0450fd98a355f22"
    d3_tip2 = "ef" * 32
    d3_start = "balances-build: start (snapshot h2208246, chain tip h2219462) — replaying /var/lib/tri7/blk0001.dat"
    d3_done = ("[2026-11-25T09:25:44Z] balances-build: done in 103.9s — coverage h2219460 tip %s · supply 226993061570 sats · "
               "18020 live UTXOs · wallet 10 keys 101643309 sats · index order → /var/lib/tri7/balances.json" % d3_tip)
    d3_stop = "balances-build: chain-ordered walk stopped at h2219461 — the chain store holds nothing at h2219461 although its chain continues above it"
    d3_retry = "balances-build: coverage h2219460 short of the chased tip h2219462 — retrying in 60s"
    d3_start2 = "balances-build: start (snapshot h2219460, chain tip h2219464) — replaying /var/lib/tri7/blk0001.dat"
    d3_done2 = ("[2026-11-25T09:27:02Z] balances-build: done in 17.8s — coverage h2219464 tip %s · supply 226993061570 sats · "
                "18020 live UTXOs · wallet 10 keys 101643309 sats · index order → /var/lib/tri7/balances.json" % d3_tip2)
    cases = [
        ("pending: index still loading", ["balances-build: waiting for the block index (Building { .. }) — the replay reads the chain in height order"], None, 3),
        ("pending: build running", [start, "balances-build: ... 1500000 blocks, 13198 live UTXOs"], None, 3),
        ("pending: build failed, scheduler retrying (no build finished yet)", [start, fail_line], None, 3),
        ("accepted", [start, good_done], agree, 0),
        ("accepted: a failed first attempt, its retry finished in index order", [start, fail_line, start, good_done], agree, 0),
        ("accepted: the chain advanced mid-walk, coverage h2219450 > the chased tip h2219400", [start, advanced_done],
         {"height": 2219450, "covered_tip_hash": tip2, "replay_target_height": 2219400}, 0),
        ("rejected: file order (pre-fix line)", [start, old_done], None, 1),
        ("rejected: file order (opt-out flag)", ["balances-build: start (snapshot h5, chain tip h9) — replaying x", good_done.replace("index order", "file order")], None, 1),
        ("rejected: short of the chased tip with no retry scheduled", [start, good_done.replace("coverage h2219400", "coverage h2208246")], None, 1),
        ("rejected: covered > chased but the snapshot stamps replay_target_height with the coverage (the wrong field)",
         [start, advanced_done], {"height": 2219450, "covered_tip_hash": tip2, "replay_target_height": 2219450}, 1),
        ("rejected: snapshot file behind the journal", [start, good_done],
         {"height": 2208246, "covered_tip_hash": "cd" * 32, "replay_target_height": 2219350}, 1),
        ("rejected: snapshot file unreadable", [start, good_done], None, 1),
        ("pending: walk capped where the store's bodies end (live tip) — the retry continues from the new snapshot", [start,
         "balances-build: chain-ordered walk stopped at h2208645 — the chain store holds nothing at h2208645 but does hold the chain above it"], None, 3),
        ("pending: a build failed and the walk capped (no build finished yet)", [start, fail_line,
         "balances-build: chain-ordered walk stopped at h2208645 — the chain store holds nothing at h2208645 but does hold the chain above it"], None, 3),
        ("rejected: a bad finished build after a failed attempt stays rejected", [start, fail_line, start, old_done], None, 1),
        ("rejected: done without a start", [good_done], agree, 1),
        ("pending: LIVE DNS3 — index-order build landed 2 blocks short of the advancing tip, the node scheduled its retry", [d3_start, d3_done, d3_stop, d3_retry], None, 3),
        ("accepted: SHAPE of the converged retry, synthetic (the live canary rolled back before its retry wrote a done line) — the retry starts from the new snapshot and reaches the tip",
         [d3_start, d3_done, d3_stop, d3_retry, d3_start2, d3_done2],
         {"height": 2219464, "covered_tip_hash": d3_tip2, "replay_target_height": 2219464}, 0),
        ("pending: short of the tip, the retry's start line already began (covered resets, the newer build is running)", [d3_start, d3_done, d3_start2], None, 3),
        ("rejected: the retry line's heights do not match the judged build (h2219450/h2219458 vs done h2219460/chased h2219462) — a different build's retry must not mark this one pending",
         [d3_start, d3_done, d3_stop, "balances-build: coverage h2219450 short of the chased tip h2219458 — retrying in 60s"], None, 1),
        ("rejected: an earlier build's retry line, seen before the judged done, does not mark the judged build pending",
         ["balances-build: coverage h2219450 short of the chased tip h2219458 — retrying in 60s", d3_start, d3_done, d3_stop], None, 1),
    ]
    bad = 0
    for name, lines, snap, want in cases:
        code, reason = judge(lines, snap)
        ok = code == want
        bad += 0 if ok else 1
        print("%s  want %d got %d  %s — %s" % ("PASS" if ok else "FAIL", want, code, name, reason))
    print("selftest: %d cases, %d failed" % (len(cases), bad))
    return 1 if bad else 0


def main(argv):
    if argv[1:] == ["--selftest"]:
        return selftest()
    epoch = None
    unit, path = "tri7d", "/var/lib/tri7/balances.json"
    rest = argv[1:]
    while rest:
        a = rest.pop(0)
        if a == "--unit" and rest:
            unit = rest.pop(0)
        elif a == "--snapshot" and rest:
            path = rest.pop(0)
        elif a.lstrip("-").isdigit() and epoch is None:
            epoch = a.lstrip("-")
        else:
            print("bad argument: %s\nusage: balances_chain_ordered.py <restart epoch> [--unit tri7d] [--snapshot PATH] | --selftest" % a)
            return 2
    if epoch is None:
        print("usage: balances_chain_ordered.py <restart epoch> [--unit tri7d] [--snapshot PATH] | --selftest")
        return 2
    pid = subprocess.run(["systemctl", "show", "-p", "MainPID", "--value", unit], capture_output=True, text=True).stdout.strip()
    if not pid.isdigit() or int(pid) == 0:
        print("%s has no main process" % unit)
        return 1
    journal = subprocess.run(
        ["journalctl", "-u", unit, "_PID=%s" % pid, "--since", "@%s" % epoch, "--no-pager", "-o", "cat"],
        capture_output=True, text=True, check=True).stdout
    try:
        with open(path, encoding="utf-8") as fh:
            snapshot = json.load(fh)
    except (OSError, ValueError):
        snapshot = None
    code, reason = judge(journal.splitlines(), snapshot)
    print(reason)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv))
