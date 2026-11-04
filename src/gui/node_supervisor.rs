//! Embedded node supervisor — the wallet works INDIVIDUALLY (Sami: "if it is
//! supposed to work individually, then you should just have it work
//! individually"). A normal user installs ONE app: the wallet spawns its own
//! node, syncs itself (progress visible in the UI), and every panel —
//! balance, history, staking, messages — reads that node. No separate daemon
//! to install or configure; the standalone tri7d remains a server tool only.
//!
//! Layout (one-folder design preserved):
//!   <wallet folder>/node/triangles.conf     generated on first run
//!   <wallet folder>/node/state/             tipstate, blk0001.dat, index/
//! The node binary sits next to the GUI exe (same install dir — the
//! installer ships both).

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

pub const DEFAULT_PEER: &str = "194.233.88.206:24113";
/// how long the supervisor waits for the node's RPC before considering a
/// spawn failed (cold index builds are slow — retry window is generous)
const RPC_WAIT: Duration = Duration::from_secs(25);
const MAX_RESTARTS: u32 = 5;

pub struct EmbeddedNode {
    pub rpc_port: u16,
    child: Option<Child>,
    snapshot_child: Option<Child>,
    snapshot_done: bool,
    started_at: Option<Instant>,
    restarts: u32,
    log_path: PathBuf,
}

impl EmbeddedNode {
    pub fn new(rpc_port: u16, log_path: PathBuf) -> Self {
        EmbeddedNode {
            rpc_port,
            child: None,
            snapshot_child: None,
            snapshot_done: false,
            started_at: None,
            restarts: 0,
            log_path,
        }
    }

    /// the node binary next to the GUI exe (installer layout)
    fn node_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let name = if cfg!(windows) {
            "follow_tip.exe"
        } else {
            "follow_tip"
        };
        let p = dir.join(name);
        if p.exists() {
            Some(p)
        } else {
            None
        }
    }

    /// Generate the node conf on first run (staking off by default — the
    /// user flips it in the UI; persisted by the daemon's staking RPC).
    fn ensure_conf(conf: &Path) {
        if conf.exists() {
            return;
        }
        if let Some(dir) = conf.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = "daemon=0\nrpc=1\n";
        let _ = std::fs::write(conf, text);
    }

    /// Ensure an embedded node is running for this wallet. Returns Ok(true)
    /// when a node answers RPC on our port (either one we spawned or one
    /// already running — e.g. a fleet node the user configured manually).
    /// Idempotent: cheap no-op when the child is already up.
    pub fn ensure_running(
        &mut self,
        wallet_path: &Path,
        node_dir: &Path,
        peers: &[&str],
    ) -> Result<bool, String> {
        // 1. already answering? (ours, or a user-started node — either is fine)
        if crate::gui::rpc_client::rpc_call(
            &format!("127.0.0.1:{}", self.rpc_port),
            "getblockcount",
            &[],
        )
        .is_ok()
        {
            return Ok(true);
        }
        // 2. our child exited? count a restart
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.child = None;
                    self.restarts += 1;
                    if self.restarts > MAX_RESTARTS {
                        return Err(format!(
                            "embedded node kept exiting ({} restarts) — log: {}",
                            self.restarts,
                            self.log_path.display()
                        ));
                    }
                }
                Ok(None) => return Ok(true), // still starting
                Err(e) => return Err(format!("node status: {}", e)),
            }
        }
        // 3. spawn
        let Some(exe) = Self::node_exe() else {
            return Err("embedded node binary (follow_tip) not found next to the wallet — reinstall".into());
        };
        let state_dir = node_dir.join("state");
        let conf = node_dir.join("triangles.conf");
        Self::ensure_conf(&conf);
        let _ = std::fs::create_dir_all(&state_dir);
        let peers_arg = peers.join(" --daemon ");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|e| format!("open node log: {}", e))?;
        let err = log
            .try_clone()
            .map_err(|e| format!("clone node log handle: {}", e))?;
        let child = std::process::Command::new(exe)
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--conf")
            .arg(&conf)
            .arg("--daemon")
            .arg(peers.first().copied().unwrap_or(DEFAULT_PEER))
            .args(
                peers
                    .iter()
                    .skip(1)
                    .flat_map(|p| ["--daemon".to_string(), p.to_string()]),
            )
            .arg("--rpc-bind")
            .arg("127.0.0.1")
            .arg("--rpc-port")
            .arg(self.rpc_port.to_string())
            .arg("--no-listen")
            .arg("--backfill")
            .arg("6000")
            .arg("--wallet")
            .arg(wallet_path)
            .arg("--forever")
            .stdout(log)
            .stderr(err)
            .spawn()
            .map_err(|e| format!("spawn embedded node: {}", e))?;
        self.child = Some(child);
        self.started_at = Some(Instant::now());
        // 4. wait for RPC to come up
        let deadline = Instant::now() + RPC_WAIT;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
            if crate::gui::rpc_client::rpc_call(
                &format!("127.0.0.1:{}", self.rpc_port),
                "getblockcount",
                &[],
            )
            .is_ok()
            {
                return Ok(true);
            }
            if let Some(c) = &mut self.child {
                if c.try_wait().map(|x| x.is_some()).unwrap_or(false) {
                    break; // died during warmup — next ensure_running restarts
                }
            }
        }
        Ok(false) // still warming (first sync/index build) — UI shows progress
    }

    /// After the embedded node's first sync completes, generate the balance /
    /// UTXO snapshots IN THE NODE'S OWN STATE DIR (Sami: a fresh install must
    /// show real balance and history without anyone running snapshot tooling
    /// by hand). Runs once; the node itself keeps its snapshots fresh in
    /// follow mode afterwards via the balances files it re-reads on restart.
    pub fn ensure_snapshots(
        &mut self,
        wallet_path: &Path,
        node_dir: &Path,
    ) -> Result<(), String> {
        if self.snapshot_done {
            return Ok(());
        }
        // a previous generation pass still running? let it finish
        if let Some(c) = &mut self.snapshot_child {
            if c.try_wait().map(|x| x.is_none()).unwrap_or(false) {
                return Ok(());
            }
            self.snapshot_child = None;
            self.snapshot_done = true; // finished (success or not — no retry storm)
            return Ok(());
        }
        let exe = Self::node_exe()
            .and_then(|p| p.parent().map(|d| d.join("balances.exe")))
            .filter(|p| p.exists())
            .unwrap_or_else(|| node_dir.join("balances.exe"));
        if !exe.exists() {
            self.snapshot_done = true; // not shipped — skip silently
            return Ok(());
        }
        let blk = node_dir.join("state").join("blk0001.dat");
        if !blk.exists() {
            return Ok(()); // node still syncing its store from nothing
        }
        // only after the node reports itself synced (getsyncinfo syncing=false
        // or the sync snapshot absent + chain advancing) — poll cheaply
        let synced = crate::gui::rpc_client::rpc_call(
            &format!("127.0.0.1:{}", self.rpc_port),
            "getsyncinfo",
            &[],
        )
        .map(|v| v.get("syncing").and_then(|s| s.as_bool()) == Some(false))
        .unwrap_or(false);
        if !synced {
            return Ok(());
        }
        let tip: u64 = crate::gui::rpc_client::rpc_call(
            &format!("127.0.0.1:{}", self.rpc_port),
            "getblockcount",
            &[],
        )
        .ok()
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
        if tip == 0 {
            return Ok(());
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|e| format!("open node log: {}", e))?;
        let child = std::process::Command::new(exe)
            .arg(&blk)
            .arg(tip.to_string())
            .arg("--allow-stale")
            .arg("--wallet")
            .arg(wallet_path)
            .current_dir(node_dir.join("state"))
            .stdout(log)
            .spawn()
            .map_err(|e| format!("spawn balances pass: {}", e))?;
        self.snapshot_child = Some(child);
        Ok(())
    }

    pub fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| c.try_wait().map(|x| x.is_none()).unwrap_or(false))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_starts_disconnected() {
        let mut n = EmbeddedNode::new(19199, std::env::temp_dir().join("ns.log"));
        assert!(!n.running());
        assert_eq!(n.rpc_port, 19199);
    }

    #[test]
    fn conf_written_once() {
        let dir = std::env::temp_dir().join(format!("nsconf{}", std::process::id()));
        let conf = dir.join("triangles.conf");
        let _ = std::fs::remove_dir_all(&dir);
        EmbeddedNode::ensure_conf(&conf);
        assert!(conf.exists());
        let text = std::fs::read_to_string(&conf).unwrap();
        assert!(text.contains("rpc=1"));
        // idempotent
        std::fs::write(&conf, "custom").unwrap();
        EmbeddedNode::ensure_conf(&conf);
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), "custom");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decision_node_exe_somewhere() {
        // on the dev box the GUI exe exists next to follow_tip; on CI it may
        // not — either way the fn must not panic
        let _ = EmbeddedNode::node_exe();
    }
}
