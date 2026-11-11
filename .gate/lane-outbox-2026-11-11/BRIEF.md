# tri7 lane: smsg outbox sender-visible decrypt (sealed message keys)

LIVE INCIDENT (Sami, phase-7 disclosed gap): outbox rows listed empty `from`/`text` placeholders — the sender could never read what they sent.

DESIGN (verified necessary deviation from "just decrypt with the sender key"): smsg's per-message `cpk_r` is an EPHEMERAL pubkey whose secret is destroyed after encryption — `ECDH(sender_priv, cpk_r)` for A→B is ECDLP, impossible. So the derived `(key_e, key_m)` are SEALED at send time under the sending secret (HMAC-SHA256 keystream XOR bound to the 18-byte row key) into a new `ekey` column; listing unseals with the owned sending key. Store access alone reveals nothing; wire format, consensus, receiver path untouched. Old pre-ekey rows keep honest placeholders (plaintext unrecoverable — disclosed). Anon sends keep placeholders by design. No legacy ShadowCoin parity claim is asserted (none verified). No new dependencies.

FILES: src/smsg.rs (+209), src/smsg_net.rs (+145). Tests: changed `outbox_rows_show_the_recipient_and_decrypt_with_the_sender_key` (real from+text now), added `outbox_sender_and_receiver_decrypt_identical_plaintext`, `outbox_row_without_the_sending_key_keeps_the_placeholder`, `outbox_anon_row_keeps_the_honest_placeholder`, `outbox_seal_roundtrip_sender_decrypt_matches_receiver` (wrong-secret MAC reject, malformed seal → None), `outbox_store_roundtrips_the_sealed_keys_column`.

PARENT-VERIFIED (fresh run in the lane tree, own target dir): `cargo test --lib` → `test result: ok. 660 passed; 0 failed; 1 ignored` (baseline 655 + 5 new).

TREE: branch lane/outbox-decrypt, worktree /root/tri7-lanes/lane-outbox, base be3cd3c; diff = `git diff -- src/smsg.rs src/smsg_net.rs` (lane.diff), stat in diffstat.txt. Repo lane only — no deployment.
