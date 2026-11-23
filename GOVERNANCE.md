# Triangles (tri7) Governance & Operations

**Status:** v1 — 2026-11-17. This document discloses who operates the
Triangles network infrastructure, what they are accountable for, and — just
as important — what the operator is **never** able to do.

## Operators of record

The network's infrastructure (seed/relay daemons, the kernel checksum oracle
feed, release engineering) is operated by **Sami Ahmed** (@sami-ahmed.net) and
the Hermes agent fleet under his direction. Today this is a small, curated
network: DNS2 (primary relay), DNS3, and the Pi node stake and relay; the
operator's Windows machine runs the wallet GUI.

This is disclosed plainly rather than hidden: the network is currently
**operator-accountable**. The roadmap (TRI7-AUDIT-2026-11-17.md, Consensus
Defense Program) exists to convert that accountability into *verifiable,
neutral* mechanisms — signed checkpoints that anchor history, never policy.

## What the operator is accountable for

1. **Release integrity.** Every release commit carries an independent review
   grade in its message; binaries are built from CI-verified trees; the fleet
   runs zero-drift (identical binaries) and consensus-relevant upgrades are
   synchronized across all nodes in one window.
2. **Kernel oracle feed.** The checksum/checkpoint oracle feed the nodes
   consume for historical kernel evidence. The feed anchors *history* — it is
   consulted for replay evidence, never to validate or veto new blocks.
3. **Incident response.** Suspected consensus incidents, splits, or exploits:
   contact the operator. Target response: acknowledge < 24 h, mitigations
   (pause/alert/fleet coordination) as fast as the failure mode demands.
   Fleet nodes pause staking automatically on detected chain-split alarms.

## What the operator is never able to do

These are design non-goals (see TRI7-AUDIT-2026-11-17.md, "Non-goals"):

- **No transaction censorship.** No code path exists — or will exist — for the
  operator to reject, reorder, or filter transactions by address, content, or
  sanctions status. Screening exists only as an *opt-in wallet-layer* feature.
- **No fund-moving authority.** There is no operator key that can spend,
  freeze, revert, or reassign anyone's coins. Checkpoints anchor history;
  they cannot move balances.
- **No permissioned admission.** Anybody may run a node; joining the relay
  set needs no permission from the operator.

## Network upgrade process

Consensus-relevant changes ship as **versioned height/bit-gated activations**:

1. A release branch carries the change with its review grade and a summary of
   the semantic delta ("this changes which blocks are valid after height H").
2. The activation height is published before any node enforces it; operators
   of downstream nodes may stay on the prior rule set (dissent = fork choice,
   documented, not punished).
3. Fleet upgrades are executed as a **synchronized zero-drift window** — all
   staking/relay nodes upgrade together, because mixed rule sets split the
   chain (demonstrated and documented in TRI7-AUDIT-2026-11-17.md).
4. Parity probes verify one canonical chain post-activation.

## Security contact

Report vulnerabilities privately to the operator contact on file
(@sami-ahmed.net). We commit to no retaliation, no TTL on disclosure
embargoes shorter than the fix, and public post-mortems for
consensus-affecting issues (this audit — TRI7-AUDIT-2026-11-17.md — is the
first such post-mortem).
