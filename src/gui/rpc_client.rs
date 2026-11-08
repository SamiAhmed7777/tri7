//! JSON-RPC client for the tri7d native surface (127.0.0.1:19114).
//! Pure std (no HTTP crate) — mirrors follow_tip.rs's probe. Used by the
//! Debug window (rpcconsole), the connection-info panel, and the passphrase
//! dialog. The allow list is deliberately RESTRICTED: reads, the staking /
//! messaging operations the GUI drives, and the wallet-file passphrase verbs
//! (encrypt / change / unlock / lock / decrypt). Anything that moves coins or
//! exports secrets (sendtoaddress, dumpprivkey, dumpwallet, import*, move) is
//! refused client-side before a byte hits the wire;
//! this client additionally restricts itself to the read-only method allow
//! list as defense in depth.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Methods the GUI may call. Mirrors the NATIVE read surface (HANDOFF 5b)
/// plus the phase-7 milestone-2 wallet-owned operations the GUI drives on the
/// user's behalf (Send screen Broadcast, staking panel on|off, Messages
/// compose) — these go to the user's OWN tri7d on loopback; the daemon still
/// validates everything server-side. Anything else → refused client-side
/// before a byte hits the wire.
pub const ALLOWED_METHODS: &[&str] = &[
    "gettip",
    "getblockcount",
    "getblockhash",
    "getblock",
    "getrawtransaction",
    "getinfo",
    "getdifficulty",
    "getconnectioncount",
    "getstakemodifier",
    "getbalance",
    "listunspent",
    "listwallettx",
    "getlocalinfo",
    "listwalletaddresses",
    "getreceivedbyaddress",
    // explorer 2026-11-07: the daemon's native rich-list verb (GUI explorer
    // prefers it, falls back to the balances.json snapshot when the node is
    // older than the verb)
    "richlist",
    "validateaddress",
    "health",
    // phase 7 milestone 2
    "getstakinginfo",
    "staking",
    "sendrawtransaction",
    "getpeerinfo",
    "smsgsend",
    "smsginbox",
    "smsgoutbox",
    "smsgscandir",
    "smsgpurge",
    "smsgaddaddress",
    "smsgkeys",
    "smsglocalinfo",
    // wallet file state (File ▸ Encrypt / Change / Unlock / Remove) — the
    // passphrase is never stored by the GUI, and the node validates it
    "encryptwallet",
    "walletpassphrase",
    "walletpassphrasechange",
    "walletlock",
    "decryptwallet",
    // the fee the NODE charges (daemon-side validated to 0..0.25 TRI, moves
    // no coins) — the Options panel pushes the typed fee there. Without it
    // on the list the whole Options apply failed with "not on the GUI
    // read-only allow list" and NO option could be applied.
    "settxfee",
];

pub fn is_allowed(method: &str) -> bool {
    ALLOWED_METHODS.contains(&method)
}

pub fn rpc_call(addr: &str, method: &str, params: &[Value]) -> Result<Value, String> {
    if !is_allowed(method) {
        return Err(format!(
            "method \"{}\" is not on the GUI read-only allow list",
            method
        ));
    }
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| format!("resolve {}: {}", addr, e))?
        .collect();
    let mut last_err = String::new();
    let mut stream = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_millis(1500)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = format!("connect {}: {}", a, e),
        }
    }
    let mut stream = stream.ok_or_else(|| last_err)?;
    stream
        // history.json can be several MB (tens of thousands of rows);
        // loopback is fast but the whole body must arrive inside one
        // read window, so allow more than a health-check timeout
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;

    let body = json!({"jsonrpc":"1.0","id":"tri7-gui","method":method,"params":params}).to_string();
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        addr,
        body.len(),
        body
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("send: {}", e))?;
    let mut text = String::new();
    stream
        .read_to_string(&mut text)
        .map_err(|e| format!("read: {}", e))?;
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&text);
    let v: Value = {
        let envelope: Value =
            serde_json::from_str(body).map_err(|e| format!("rpc json: {} — {:?}", e, body))?;
        let err = envelope.get("error").filter(|e| !e.is_null());
        if let Some(e) = err {
            return Err(format!("rpc error: {}", e));
        }
        envelope.get("result").cloned().unwrap_or(Value::Null)
    };
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn rpc_call_parses_canned_response_and_enforces_allowlist() {
        // allow-list enforcement happens before any I/O
        assert!(!is_allowed("sendtoaddress"));
        assert!(!is_allowed("dumpprivkey"));
        assert!(!is_allowed("move"));
        // the passphrase verbs ARE reachable: the Encrypt dialog drives them
        assert!(is_allowed("walletpassphrase"));
        assert!(is_allowed("encryptwallet"));
        assert!(is_allowed("walletpassphrasechange"));
        assert!(is_allowed("decryptwallet"));
        assert!(is_allowed("getconnectioncount"));
        assert!(rpc_call("127.0.0.1:1", "sendtoaddress", &[]).is_err());

        // canned loopback server: exactly the tri7d envelope
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = r#"{"result":{"connections":1},"error":null}"#;
        let http = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut s, &mut buf);
                let _ = s.write_all(http.as_bytes());
            }
        });
        let v = rpc_call(&format!("127.0.0.1:{}", port), "getconnectioncount", &[])
            .expect("canned call must parse");
        assert_eq!(v["connections"], 1);
    }

    #[test]
    fn rpc_call_unreachable_is_clean_error() {
        let err = rpc_call("127.0.0.1:9", "getblockcount", &[]).unwrap_err();
        assert!(err.contains("connect"), "unexpected error: {}", err);
    }
}
