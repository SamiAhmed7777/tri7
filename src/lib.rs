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
pub mod gui;
pub mod kernel;
pub mod legacy;
pub mod p2p;
pub mod replay;
pub mod rpc;
pub mod spend;
pub mod stake;
pub mod wallet;

pub use legacy::{Block, Tx, Vin, Vout};
