# Add Seed Phrase works on zero-balance wallets (phrase-only mode)

Operator: "Fix that button though because people will need it" — on any
wallet without spendable UTXOs (fresh install, empty wallet, immature
stakes), clicking "Add seed phrase" refused with "no spendable UTXOs …
nothing to migrate" (surfaced only in a status log), which reads as a dead
button.

Fix — the wizard now COMPLETES on zero-balance wallets instead of refusing:
- begin_seed_migration_action: the balance pre-check is removed; the wizard
  opens unconditionally (still gated on loaded wallet, writability, and the
  seedmigrated marker).
- SweepFlow::prepare: with empty inputs the flow still derives the phrase's
  destinations, roundtrip-verifies them, inserts the 10 derived keys into
  wallet.dat, advances the HD chain counter, and sets the default key — then
  completes as a NEW step PhraseAdded{keys_inserted, keys_already_present}
  with NO transaction and NO 'seedmigrated' marker (so if coins later arrive
  on the old random keys, re-running the wizard locks to the stored phrase
  and CAN sweep them).
- UI renders the PhraseAdded step as a success card explaining exactly that.
- Tests updated: the two old refusal assertions now assert the new
  phrase-only completion (491 lib tests green).

cargo test --release --lib: 491 passed, 0 failed.
