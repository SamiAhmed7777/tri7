# Balance card: full five-row layout restored, node-anchored

Operator: "What happened to the other fields? I don't like this" — the
previous fix replaced the whole card with a two-row daemon figure, dropping
the Stake/Unconfirmed/Immature rows he expects.

Now: the classic layout is fully restored (Spendable / Stake / Unconfirmed /
Immature / divider / Total). Spendable and Total are the daemon's live
getbalance when the node answers (authoritative — fixes the drifting
3036-vs-node figure); the three maturing rows keep the local split's figures
and the card carries the note "Spendable and Total are the node's live
getbalance; maturing rows are the local view". Offline: unchanged legacy
behavior.

cargo test --release --lib: 491 passed, 0 failed.
