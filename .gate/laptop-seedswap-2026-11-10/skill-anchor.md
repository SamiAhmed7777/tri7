# Target: /root/.hermes/skills/tri7-fleet-chain-state/SKILL.md (NOT /root/tri7/SKILL.md)
# Anchor uniqueness: grep -c 'Never launch a Windows daemon inline over SSH' SKILL.md -> 1

--- EXACT FILE REGION around the insertion point (section 6, laptop topology) ---
310-(46.62.249.20:443) — samihost's 24113 times out from that network.
311-
312:**Never launch a Windows daemon inline over SSH, and never hand-write its
313-scheduled task from an SSH command line.** A process launched from an SSH
314-session dies when the session exits; and bash escaping lands DOUBLED
315-backslashes in the task's `Task To Run`, which fails with `Last Result: 2`
316-(file not found) while the command reports SUCCESS. Write the daemon batch
