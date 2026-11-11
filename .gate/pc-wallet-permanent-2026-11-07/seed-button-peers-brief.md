# Seed-migration button honesty + 443 bootstrap peer (restricted networks)

Two operator-reported issues from live GUI use:

1. "Add Seed phrase button doesn't work" — on an already-migrated wallet the
   button opened the flow only to refuse it deep inside
   begin_seed_migration_action ("already completed seed migration"), which
   reads to the operator as a dead button. Both call sites (Options/Wallet and
   Receive screens) now check wallet.seed_migrated first and render a Success
   notice ("This wallet already has a seed phrase — the balance lives on the
   phrase's own derived addresses; adding another would double-sweep")
   INSTEAD of the clickable button. Unmigrated wallets keep the button.

2. "laptop has no connections to the network" — the laptop's local web filter
   allows ONLY outbound 80/443 (verified: 80->True, 8080/9050/19112/24113->
   False, samihost:443->True). Fresh installs bootstrapped from
   samihost:24113, which the filter drops, so the embedded node sat dead
   ("needs a live peer"). Changes:
   - DEFAULT_PEER -> 46.62.249.20:443 (the Pi's new clearnet listener on 443;
     ufw 443/tcp opened, listener verified externally OPEN from DNS2 and
     True from the filtered laptop itself).
   - FALLBACK_PEER added: samihost 194.163.161.162:24113; the supervisor
     now passes both peers to the spawned daemon.

cargo test --release --lib: 455 passed, 0 failed.
Evidence: this diff (two files), live connectivity probes quoted above.
