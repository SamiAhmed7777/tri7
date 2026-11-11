        "addmultisigaddress" => {
            // legacy rpcwallet.cpp:836: build an m-of-n P2SH multisig address
            // from Triangles addresses / hex pubkeys, store the redeem script
            // (AddCScript) and return the bare address string. Every legacy
            // failure here is std::runtime_error, which the v5 dispatcher
            // (trianglesrpc.cpp:1321) rethrows as JSONRPCError(RPC_MISC_ERROR)
            // → error code -1 carrying the verbatim string.
            let n_required = match param_get(params, 0, "nrequired") {
                // legacy get_int(): a negative nrequired parses and must fall
                // into the "at least one key" check below
                Some(Value::Number(n)) => n.as_i64(),
                _ => None,
            };
            let Some(n_required) = n_required else {
                return NativeOutcome::Err(-8, "expected nrequired parameter".into());
            };
            let keys_param = match param_get(params, 1, "keys") {
                Some(Value::Array(a)) => a.clone(),
                _ => return NativeOutcome::Err(-8, "expected an array of keys".into()),
            };
            let account = ptry!(p_str(params, 2, "account")).unwrap_or_default();
            // legacy check order (rpcwallet.cpp:851): nrequired < 1 first
            if n_required < 1 {
                return NativeOutcome::Err(
                    -1,
                    "a multisignature address must require at least one key to redeem".into(),
                );
            }
            // ... then the keys count (rpcwallet.cpp:855)
            if (keys_param.len() as i64) < n_required {
                return NativeOutcome::Err(
                    -1,
                    format!(
                        "not enough keys supplied (got {} keys, but need at least {} to redeem)",
                        keys_param.len(),
                        n_required
                    ),
                );
            }
            let path = match wallet_file_path() {
                Some(p) => p,
                None => {
                    return NativeOutcome::Err(
                        -1,
                        "wallet file path unknown to this node — start tri7d with --wallet"
                            .into(),
                    )
                }
            };
            // Gather public keys (legacy case 1: Triangles address we hold
            // the full pubkey for; case 2: hex-encoded public key).
            let wallet_keys = match crate::wallet::read_wallet(&path) {
                Ok(w) => w.keys,
                Err(e) => {
                    return NativeOutcome::Err(
                        -1,
                        format!("addmultisigaddress: cannot read wallet: {e}"),
                    )
                }
            };
            let mut pubkeys: Vec<Vec<u8>> = Vec::with_capacity(keys_param.len());
            for k in &keys_param {
                let Some(ks) = k.as_str() else {
                    // legacy keys[i].get_str() throws → -1 via the dispatcher
                    return NativeOutcome::Err(-1, "Invalid public key".into());
                };
                if let Ok(payload) = crate::address::address_to_payload(ks) {
                    // a valid address: must be a P2PKH 'T' address referring
                    // to a key the wallet holds the full pubkey for
                    if payload[0] != crate::address::PUBKEY_ADDRESS {
                        return NativeOutcome::Err(
                            -1,
                            format!("{ks} does not refer to a key"),
                        );
                    }
                    match wallet_keys.iter().find(|wk| wk.address == ks) {
                        Some(wk) => pubkeys.push(wk.pubkey.clone()),
                        None => {
                            return NativeOutcome::Err(
                                -1,
                                format!("no full public key for address {ks}"),
                            )
                        }
                    }
                } else if let Ok(pk) = crate::p2p::hex_decode(ks) {
                    // legacy throws " Invalid public key: "+ks — the LEADING
                    // SPACE is in the v5 source (rpcwallet.cpp:882), keep it
                    // byte-for-byte
                    if secp256k1::PublicKey::from_slice(&pk).is_err() {
                        return NativeOutcome::Err(
                            -1,
                            format!(" Invalid public key: {ks}"),
                        );
                    }
                    pubkeys.push(pk);
                } else {
                    return NativeOutcome::Err(-1, format!(" Invalid public key: {ks}"));
                }
            }

            // The 16-key cap: legacy SetMultisig/EncodeOP_N is
            // assert(n >= 0 && n <= 16) — it ABORTS the process. tri7 refuses
            // instead (a deliberate, documented deviation from legacy).
            // Checked AFTER the legacy-ordered checks above so e.g.
            // nrequired=17 with 2 keys still answers "not enough keys".
            if n_required > 16 || pubkeys.len() > 16 {
                return NativeOutcome::Err(
                    -8,
                    "a multisignature address must require between 1 and 16 keys".into(),
                );
            }
            let redeem = match crate::multisig::redeem_script(n_required as u8, &pubkeys) {
                Ok(r) => r,
                Err(e) => return NativeOutcome::Err(-1, e),
            };
            let address = crate::multisig::p2sh_address(&redeem);
            let mut wh = match crate::wallet::WalletHandle::open_readwrite(&path) {
                Ok(w) => w,
                Ok(w) => w,
                Err(e) => {
                    return NativeOutcome::Err(
                        -1,
                        format!("addmultisigaddress: cannot open wallet: {e}"),
                    )
                }
            };
            if let Err(e) = wh.put_redeem_script(&redeem) {
                wh.close();
                return NativeOutcome::Err(-1, format!("addmultisigaddress: {e}"));
            }
            // legacy SetAddressBookName(innerID, strAccount) right after
            // AddCScript; an empty account name erases the row in the
            // original client, so a missing/empty [account] writes nothing.
            if !account.is_empty() {
                if let Err(e) = wh.set_label(&address, &account) {
                    wh.close();
                    return NativeOutcome::Err(-1, format!("addmultisigaddress: {e}"));
                }
            }
            wh.close();
            NativeOutcome::Ok(json!(address))
        }

        "addredeemscript" => {
            // legacy rpcwallet.cpp:905: store a hex redeem script in the
            // wallet (AddCScript) so its P2SH address becomes spendable.
            // Returns the bare P2SH address string (CTrianglesAddress).
            let Some(hex) = ptry!(p_str(params, 0, "redeemScript")) else {
                return NativeOutcome::Err(-8, "expected redeemScript parameter".into());
            };
            let account = ptry!(p_str(params, 1, "account")).unwrap_or_default();
            let script = match crate::p2p::hex_decode(&hex) {
                Ok(s) => s,
                // legacy ParseHexV (trianglesrpc.cpp:151):
                // RPC_INVALID_PARAMETER with the verbatim format string
                Err(_) => {
                    return NativeOutcome::Err(
                        -8,
                        format!("redeemScript must be hexadecimal string (not '{hex}')"),
                    )
                }
            };
            let path = match wallet_file_path() {
                Some(p) => p,
                None => {
                    return NativeOutcome::Err(
                        -1,
                        "wallet file path unknown to this node — start tri7d with --wallet"
                            .into(),
                    )
                }
            };
            let mut wh = match crate::wallet::WalletHandle::open_readwrite(&path) {
                Ok(w) => w,
                Err(e) => {
                    return NativeOutcome::Err(-1, format!("addredeemscript: cannot open wallet: {e}"))
                }
            };
            if let Err(e) = wh.put_redeem_script(&script) {
                wh.close();
                return NativeOutcome::Err(-1, format!("addredeemscript: {e}"));
            }
            // legacy SetAddressBookName(innerID, strAccount) — see
            // addmultisigaddress above for the empty-name note
            if !account.is_empty() {
                let addr = crate::multisig::p2sh_address(&script);
                if let Err(e) = wh.set_label(&addr, &account) {
                    wh.close();
                    return NativeOutcome::Err(-1, format!("addredeemscript: {e}"));
                }
            }
            wh.close();
            NativeOutcome::Ok(json!(crate::multisig::p2sh_address(&script)))
        }

        // Ops verbs that may run before the live wallet view is ready (a pure
        // codec, the wallet FILE, or the process) answer from the passive
        // layer, so "warming up" never blocks work that is already possible.
        // Arms here RETURN: nothing re-enters the dispatch.
