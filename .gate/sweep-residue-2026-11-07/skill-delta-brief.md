# Skill delta: SWEEP-RESIDUE floor documented in triangles-modernization

Scope: 2 files in the skill, both additive —
`SKILL.md` (+1 pointer line in "TRI7 ecosystem packaging", mirroring the
KEYPOOLREFILL/EXPLORER-VERB entries) and
`references/tri7-cli-and-seed-tools.md` (sweep CLI line gains `--min-sats`;
+1 "Residue floor" section). No existing rule removed or reworded.

DEFINITION: "additive" = the diff only inserts lines and extends one flag
list; grep for the removed strings below returns nothing.

### CMD: `grep -n "SWEEP-RESIDURE" SKILL.md | head -2` (typo-guard: the real grep)
```
165:- SWEEP-RESIDUE (2026-11-07, commit 871dbf3, GLM A): CLI sweep `--min-sats` residue floor (default 5000 sats = 0.005 TRI) — see references/tri7-cli-and-seed-tools.md for the floor rule, the CLI-only scope (GUI sweep_flow keeps move-everything), and the live two-pass completion proof (non-seed 582.25 → 0 TRI, staking never paused).
```

### CMD: `grep -c "min-sats" references/tri7-cli-and-seed-tools.md`
```
2
```
(The flag appears in the CLI inventory line and in the new Residue floor
section — no other sweep doc claims a flag that does not exist; the shipped
binary answers `tri7 sweep --help` with `--min-sats [N]` default 5000,
commit 871dbf3.)

Facts encoded, all from the live run this tick: floor default 5000 sats,
`split_residue_skip` semantics (floor-inclusive keep, `min_sats <= 0` keeps
all), CLI-only scope (GUI sweep_flow.rs untouched — move-everything contract
preserved), and the two-pass live completion (56-input 555.77 TRI pass mined
h2207308; re-stake mid-pass left one immature coin; matured-tail pass mined
h2207311; non-seed 582.25 → 0 TRI; staking enabled throughout).
