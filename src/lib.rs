//! tri7-core: modern full-node core for the Triangles chain.
//!
//! Phase 1 goal: byte-exact replay of the legacy chain. Everything in this
//! module mirrors the wire format of triangles_v5 (the 2013-era C++ client)
//! so that the same bytes produce the same hashes and the same chain.

pub mod address;
pub mod blkfile;
pub mod blockindex;
pub mod consensus;
pub mod difficulty;
pub mod forkchoice;
pub mod history;
pub mod gui;
pub mod kernel;
pub mod keytool;
pub mod legacy;
pub mod legacy_rows;
pub mod multisig;
pub mod p2p;
pub mod rawtx;
pub mod replay;
pub mod rpc;
pub mod seed;
pub mod shutdown;
pub mod smsg;
pub mod sync_progress;
pub mod smsg_net;
pub mod spend;
pub mod walletcrypto;
pub mod wallet_encrypt;
pub mod addrindex;
pub mod bdb_import;
pub mod onion;
pub mod peers;
pub mod stake;
pub mod sweep;
pub mod txpool;
pub mod updater;
pub mod wallet;

pub use legacy::{Block, Tx, Vin, Vout};
