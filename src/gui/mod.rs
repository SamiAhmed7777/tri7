//! tri7 GUI wallet (phase 6) — the legacy triangles-qt experience on the
//! tri7-core engine.
//!
//! Layering (display-independent by construction):
//! - [`state`] pure helpers: amounts, addresses, UTXO derivation (snapshot +
//!   live-tail reconciliation), signed sends via spend.rs, QR/PNG, message
//!   signing, staking/chain panels.
//! - [`app_state`] the AppState machine (screens/actions) + `--selfcheck`.
//! - [`rpc_client`] read-only JSON-RPC to the local tri7d (Debug window).
//! - [`app`] (feature `gui`) the thin egui view reproducing the legacy
//!   triangles-qt identity: Black-Pharao palette (#000 / #1c1c1c / #e32105),
//!   the original menu_16 icons and header logo, page set and labels carried
//!   over from triangles_v5/src/qt (see assets/ for the copied PNGs).
//!
//! NO network broadcast exists anywhere in this module. The only outbound
//! traffic is read-only JSON-RPC to the local tri7d (127.0.0.1:19114).

pub mod app_state;
pub mod explorer;
pub mod rpc_client;
pub mod seed_flow;
pub mod state;
pub mod sweep_flow;

#[cfg(test)]
mod tests;

#[cfg(feature = "gui")]
pub mod app;
#[cfg(feature = "gui")]
pub mod theme;
