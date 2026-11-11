# Overview balance shows the node's getbalance — one number, live

Operator: "it says 3036 what the f---" while the node held 3,382.70 and
climbing. Root cause: the Overview card rendered the LOCAL snapshot split
(AppState::balance_split) as the primary number and demoted the node's
getbalance to a hidden note. During heavy staking churn (152 UTXOs, the
oldest 430 conf — every coin recycles constantly) the local snapshot's
coverage lags the node, so the displayed figure drifted downward (3319 ->
3036) while the true total grew.

Fix: when debug_info.daemon_balance_sats is present (RPC reachable), the
card renders the DAEMON's getbalance as Spendable and Total with the note
"authoritative: node getbalance — updates live as stakes mature". The local
split remains the fallback for wallet-only / offline use.

cargo test --release --lib: 491 passed, 0 failed.
Live evidence: node getbalance 3382.703595 vs displayed 3036.990591 (quoted
in-session); 152 UTXOs, max conf 430 (churn quote above).
