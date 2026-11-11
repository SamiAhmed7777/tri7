# Skill delta — tri7-fleet-chain-state §16 (workqueue item 19, EVIDENCE-EQUAL-HEIGHT)

Base skill: /root/.hermes/skills/tri7-fleet-chain-state/SKILL.md, section 16
("Staking races are normal; serving truth has three layers (item 18)"), last
bullet is "Deploying serving-truth fixes requires ALL serving peers on the
same binary…". One new bullet is appended to §16 (text below, verbatim as it
will land). No other section changes.

## Why

Live fleet catch 2026-11-10 05:3xZ on sami-pc (wallet home): the item-18
empty-reply fork-evidence trigger gated on the peer's handshake start_height
being ABOVE our tip — but start_height is frozen at handshake, and the PC's
session handshook at EQUAL height (2208769 == 2208769) before the fork grew,
so the trigger was structurally unable to fire and the PC sat 25+ minutes on
a 9-block fork. Fix (commit e42c79b, graded A
urn:ump:lmyquutstd7go6y32ixwnve4sa5sy5eaqbnpwexrswprqf444afa): the evidence
also fires on 3 consecutive empty tip-poll replies regardless of start_height.
This skill bullet records the durable lesson for future ticks.

## The bullet (verbatim)

- (item 19, 2026-11-10) **start_height is FROZEN AT HANDSHAKE** — any
  height-gated trigger on it can never fire on a session that handshook at
  EQUAL height and forked later (live: sami-pc sat 25+ min on a 9-block
  fork, 2208769==2208769 at handshake, while the item-18 evidence waited for
  a taller peer that would never exist). The empty-reply evidence now ALSO
  fires on `EMPTY_REPLY_EVIDENCE_STREAK = 3` consecutive empty tip-poll
  replies REGARDLESS of start_height (~90 s at POLL_SECS=30); the fire log
  names its trigger ("start-height" / "streak" / "both"). OPS FACT
  (unchanged, now belt-and-braces): a follower stuck with NO judge activity
  → compare the peer's start_height in getpeerinfo vs our height; a daemon
  restart (fresh handshake, peer now higher) unsticks it immediately.
  Corollary: EVERY window-locator re-ask must set awaiting_window_reply —
  with a streak counter, an unmarked re-ask's empty echoes count as
  evidence and fire spurious duplicate re-asks (the mixed-batch and
  unknown-inv sends were silently unmarked until item 19).

## Evidence

- Code + regression tests (equal-at-handshake shape):
  /root/tri7/.gate/equal-height-evidence-2026-11-10/ (BRIEF.md, diff.patch,
  gate-log.txt); GLM r1 A + r2 A zero blockers.
- Live incident: workqueue TRI7-WORKQUEUE.md PROGRESS 2026-11-10 05:3x-05:5xZ
  entry (fork found, ops-repaired via restart, item 19 created).
