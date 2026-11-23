===== EXCERPT: src/node_embedded.rs lines 2600-2640 — WindowOutputs::rebase — FIX (a): drop below-window metas =====
2600	    }
2601	
2602	    /// Shift every stored window-relative height down after the window base
2603	    /// advanced (ChainState::trim re-basing; keeps kernel-meta consumers
2604	    /// aligned with entries' relative heights in-session).
2605	    ///
2606	    /// item 37 STALE-RESOLVER-OFFBYONE: a coin whose containing block fell
2607	    /// below the NEW base (relative height < excess) has NO evidence slot in
2608	    /// the slid window — clamping it to 0 (the old saturating_sub) silently
2609	    /// re-pointed its meta at entries[0], i.e. the block AT the new base, so
2610	    /// resolve_stake_evidence compared the coin's facts against a block ABOVE
2611	    /// its own (live DNS2 2026-11-21: exactly H+1's block_time at conf ==
2612	    /// WINDOW_ENTRIES) and flagged a false facts-mismatch no rebuild could
2613	    /// clear. Such a coin is past the staking horizon by design (the item-24
2614	    /// classification), so DROP it here: kernel_meta → None → the resolver's
2615	    /// below-base branch counts it unresolvable (a kernel-coverage fact —
2616	    /// never actionable, never fed to the repairwallet remedy) and the
2617	    /// validator's None path already rejects such evidence honestly.
2618	    fn rebase(&mut self, excess: usize) {
2619	        if excess == 0 {
2620	            return;
2621	        }
2622	        self.map.retain(|_, m| m.height >= excess);
2623	        for m in self.map.values_mut() {
2624	            m.height = m.height.saturating_sub(excess);
2625	        }
2626	    }
2627	}
2628	
2629	impl KernelOutputResolver for WindowOutputs {
2630	    fn kernel_meta(&self, key: &(Hash, u32)) -> Option<UtxoMeta> {
2631	        self.map.get(key).copied()
2632	    }
2633	}
2634	
2635	struct ChainState {
2636	    base: u64, // absolute height of entries[0]
2637	    entries: Vec<IndexEntry>,
2638	    checksums: Vec<u32>, // per-entry stake-modifier checksum, aligned to entries
2639	    utxo: WindowOutputs,
2640	    records: Vec<BlockRec>, // full-tx records for the last WINDOW_BLOCKS heights

===== EXCERPT: src/node_embedded.rs lines 8483-8510 — StakeState — new latch + rate-limit fields =====
8483	    /// and how many have fired in it (capped; a fresh episode re-arms).
8484	    /// IN-MEMORY ONLY (this struct never persists): a daemon restart
8485	    /// mid-episode re-arms the budget — bounded and benign (≤3 more
8486	    /// bounded rebuild passes per restart), never worth persistence.
8487	    autoheal_episode: Option<String>,
8488	    autoheal_attempts: u32,
8489	    /// item 37 STALE-RESOLVER-OFFBYONE: the episode (`first_utc`) whose
8490	    /// in-process rebuild returned ZERO owned-output mismatches while the
8491	    /// stale flag kept firing — the RESOLVER-fault latch. The repairwallet
8492	    /// remedy is a proven no-op for that class, so the episode's attempt
8493	    /// budget is retired (no further rebuilds until a fresh episode) and
8494	    /// the STALE line's remedy text stops recommending repairwallet.
8495	    /// IN-MEMORY ONLY (this struct never persists).
8496	    autoheal_fault_episode: Option<String>,
8497	    /// item 37: consecutive stake ticks with ≥1 unresolvable candidate
8498	    /// (below-base / out-of-horizon coins — a growing, permanent set once a
8499	    /// coin crosses the WINDOW_ENTRIES edge). Rate-limits the
8500	    /// kernel-coverage log line to the house cadence (first tick, then every
8501	    /// 40th ≈10 min) so aging-out coins don't spam the journal every 15s.
8502	    /// IN-MEMORY ONLY.
8503	    unresolvable_ticks: u64,
8504	}
8505	
8506	type SharedStake = Arc<Mutex<StakeState>>;
8507	type SharedWalletLive = Arc<Mutex<Option<WalletLive>>>;
8508	
8509	/// Live-preferring wallet provider (Sami: "why am I not seeing the number go
8510	/// up or the staking notifications"): getbalance serves the LIVE window view

===== EXCERPT: src/node_embedded.rs lines 9255-9350 — note_stale_evidence — FIX (d): latch-aware remedy + honest CLEARED =====
9255	/// default 15s stake cadence) — loud forever, spam never. The tick itself
9256	/// proceeds WITHOUT the phantom coins.
9257	///
9258	/// Called BEFORE stake_tick's locked/reserve early returns (round-1 judge
9259	/// blocker): those gates used to return before this ran, so a stale live
9260	/// view went undetected — or a stale flag stuck uncleared — for exactly as
9261	/// long as the wallet could not stake anyway.
9262	fn note_stale_evidence(
9263	    stg: &mut ChainState,
9264	    stale_coins: &[StaleLiveView],
9265	    fault_episode: Option<&str>,
9266	) {
9267	    if stale_coins.is_empty() {
9268	        if stg.staking_impaired.is_some() {
9269	            log(
9270	                "stake: stale live view CLEARED — no live-view coin contradicts the chain \
9271	                 anymore (a repairwallet/autoheal rebuild took effect, or the flagged \
9272	                 coin(s) left the staking window — the AUTOHEAL/resolver log tells which)",
9273	            );
9274	        }
9275	        stg.staking_impaired = None;
9276	        return;
9277	    }
9278	    let coins = stale_coins.len();
9279	    let first = &stale_coins[0];
9280	    // item 37 STALE-RESOLVER-OFFBYONE: when this episode's in-process rebuild
9281	    // already returned ZERO owned-output mismatches, the flag is a resolver
9282	    // fault (the wallet was never wrong) and the remedy text must NOT point
9283	    // at repairwallet — that advice turned DNS2's false flag into a repair
9284	    // recommendation for a proven no-op.
9285	    let resolver_fault = fault_episode == Some(first.first_utc.as_str());
9286	    let impaired = stg.staking_impaired.get_or_insert_with(|| first.clone());
9287	    impaired.coins = coins;
9288	    impaired.ticks += 1;
9289	    if impaired.ticks == 1 || impaired.ticks % 40 == 0 {
9290	        let why = if impaired.absent {
9291	            // the absent-from-canonical-map shape: the chain threads the
9292	            // claimed height AND the claimed height sits at/above the kernel
9293	            // map's coverage floor, so the map claims that era too — an
9294	            // abandoned-fork output the rollback dropped from the canonical
9295	            // map (a true phantom; a below-floor absence never reaches this
9296	            // branch — it is counted unresolvable, item 24)
9297	            format!(
9298	                "coin {}:{} claims containing block h{} (block_time {}, tx_time {}) but that \
9299	                 outpoint is ABSENT from the canonical UTXO map although h{} is inside the \
9300	                 threaded window and at/above the kernel coverage floor (window-rel {}, \
9301	                 chain holds block_time {} there)",
9302	                impaired.txid,
9303	                impaired.n,
9304	                impaired.wallet_height,
9305	                impaired.wallet_block_time,
9306	                impaired.wallet_tx_time,
9307	                impaired.wallet_height,
9308	                stg.utxo_floor_rel,
9309	                impaired.chain_block_time,
9310	            )
9311	        } else {
9312	            format!(
9313	                "coin {}:{} claims containing block h{} (block_time {}, tx_time {}) but the \
9314	                 chain holds block_time {} / tx_time {} at that height",
9315	                impaired.txid,
9316	                impaired.n,
9317	                impaired.wallet_height,
9318	                impaired.wallet_block_time,
9319	                impaired.wallet_tx_time,
9320	                impaired.chain_block_time,
9321	                impaired.chain_tx_time,
9322	            )
9323	        };
9324	        let remedy = if resolver_fault {
9325	            "the in-process rebuild returned ZERO owned-output mismatches — RESOLVER FAULT \
9326	             (item 37), NOT wallet corruption: do NOT run repairwallet, look at the \
9327	             resolver's chain-evidence lookup"
9328	        } else {
9329	            "A fork adoption does not roll the wallet live view back: run `repairwallet` \
9330	             to rebuild it from the store."
9331	        };
9332	        log(&format!(
9333	            "STAKE: STALE LIVE VIEW — {}; \
9334	             wallet evidence is not chain evidence — {} coin(s) excluded from staking. {}",
9335	            why, coins, remedy,
9336	        ));
9337	    }
9338	}
9339	
9340	/// item 27 §16-RACED-MINT-AUTOHEAL: max in-process repairwallet passes per
9341	/// impairment episode. Three matches the documented live heal discipline
9342	/// (a pass can lag the detector, and a re-arming phantom can need a second
9343	/// pass); beyond that the condition is NOT self-healing (store behind the
9344	/// tip, a persistent fork) and an operator must look — the
9345	/// STAKE: STALE LIVE VIEW line keeps firing on its loud cadence either way.
9346	const AUTOHEAL_MAX_ATTEMPTS: u32 = 3;
9347	
9348	/// Should this tick fire the in-process repair attempt? Pure policy so the
9349	/// cadence/cap tests need no store: fires on the impairment's loud re-log
9350	/// cadence (first sighting, then every 40th tick ≈10 min at the default

===== EXCERPT: src/node_embedded.rs lines 9355-9380 — autoheal_due — FIX (c): fault latch gates the cadence =====
9355	    if stake_st.autoheal_episode.as_deref() != Some(impaired.first_utc.as_str()) {
9356	        stake_st.autoheal_episode = Some(impaired.first_utc.clone());
9357	        stake_st.autoheal_attempts = 0;
9358	        stake_st.autoheal_fault_episode = None;
9359	    }
9360	    // item 37 STALE-RESOLVER-OFFBYONE: an episode whose rebuild already
9361	    // returned ZERO owned-output mismatches while the flag kept firing is
9362	    // a RESOLVER fault — the wallet was never wrong and the repairwallet
9363	    // remedy is a proven no-op for that class. Retire the episode: no
9364	    // further attempts until a fresh episode (new first_utc) re-arms.
9365	    if stake_st.autoheal_fault_episode.as_deref() == Some(impaired.first_utc.as_str()) {
9366	        return false;
9367	    }
9368	    if stake_st.autoheal_attempts >= AUTOHEAL_MAX_ATTEMPTS {
9369	        return false;
9370	    }
9371	    impaired.ticks == 1 || impaired.ticks % 40 == 0
9372	}
9373	
9374	/// One in-process `repairwallet` attempt, fired by the stake tick when its
9375	/// stale-evidence determination is live (item 27 §16-RACED-MINT-AUTOHEAL).
9376	///
9377	/// WHY: a secondary staker that races a heavier branch (item 17 by design,
9378	/// races are SAFE) holds coins minted in its own abandoned block; the fork
9379	/// adoption rolls back the chain but NOT the wallet live view, the resolver
9380	/// excludes the raced coins, and staking starves until an operator runs

===== EXCERPT: src/node_embedded.rs lines 9395-9465 — autoheal_stale_live_view — FIX (c): rebuild-clean latch, no burn, no swap =====
9395	fn autoheal_stale_live_view(
9396	    stg: &mut ChainState,
9397	    wl: &mut WalletLive,
9398	    store: &BlockStore,
9399	    stake_st: &mut StakeState,
9400	) -> bool {
9401	    let Some(impaired) = stg.staking_impaired.as_ref() else {
9402	        return false;
9403	    };
9404	    if !autoheal_due(impaired, stake_st) {
9405	        return false;
9406	    }
9407	    // the exact repairwallet replay window (RPC verb): base = min of the
9408	    // view's seed base and the chain-state base — a forkchoice rollback can
9409	    // walk the chain-state base back below the view's seed base, and
9410	    // replaying only from the view's base would then miss the restored
9411	    // branch; tip = the chain tip
9412	    let base = wl.base.min(stg.base);
9413	    let tip_hint = stg.tip_height;
9414	    let has_body = |h: u64| store.block_by_height(h).ok().flatten().is_some();
9415	    let (replay_tip, back) = newest_replayable_tip(&has_body, base, tip_hint);
9416	    if replay_tip <= base {
9417	        // store behind the view — transient (bodies still landing); the
9418	        // next re-log tick retries WITHOUT burning the attempt budget
9419	        log(&format!(
9420	            "stake: AUTOHEAL skipped — no stored bodies in h{base}..h{tip_hint} — store behind the tip, retry on the next re-log tick"
9421	        ));
9422	        return false;
9423	    }
9424	    let fresh = match WalletLive::seed(store, wl.wallet.clone(), base, replay_tip) {
9425	        Ok(f) => f,
9426	        Err(e) => {
9427	            log(&format!(
9428	                "stake: AUTOHEAL replay failed: {e} — retry on the next re-log tick"
9429	            ));
9430	            return false;
9431	        }
9432	    };
9433	    let diff = {
9434	        let owns = |o: &LiveOut| wl.owns(o);
9435	        repair_diff(&wl.live, &fresh.live, &owns)
9436	    };
9437	    // item 37 STALE-RESOLVER-OFFBYONE: a rebuild that finds ZERO owned-output
9438	    // mismatches while the stale flag still fires (this pass only ran because
9439	    // stale_coins was non-empty) proves the RESOLVER at fault — the wallet
9440	    // was never wrong, and re-swapping an identical view heals nothing. Log
9441	    // it LOUDLY ONCE per episode, retire the episode's attempt budget (no
9442	    // burn, no swap), and leave the remedy to the resolver's own fix. Live
9443	    // DNS2 2026-11-21: three silent no-op rebuilds ("0 owned-output
9444	    // mismatch(es) totalling 0 sats") burned the full item-27 episode cap
9445	    // feeding a false flag that ran for hours.
9446	    if diff.count() == 0 {
9447	        if stake_st.autoheal_fault_episode.as_deref() != Some(impaired.first_utc.as_str()) {
9448	            log(&format!(
9449	                "stake: AUTOHEAL — rebuild h{base}..h{replay_tip} found ZERO owned-output mismatch(es) while the stale flag still fires: RESOLVER FAULT (item 37), NOT wallet corruption — the repairwallet remedy is a no-op for this class; this episode's attempt budget is retired, look at the resolver's chain-evidence lookup, not the wallet"
9450	            ));
9451	            stake_st.autoheal_fault_episode = Some(impaired.first_utc.clone());
9452	        }
9453	        return false;
9454	    }
9455	    stake_st.autoheal_attempts += 1;
9456	    log(&format!(
9457	        "stake: AUTOHEAL — ran the repairwallet remedy in-process: rebuilt live view h{base}..h{replay_tip} (walked back {back} from tip h{tip_hint}) — {} owned-output mismatch(es) totalling {} sats ({} phantom, {} lost)",
9458	        diff.count(),
9459	        diff.amount(),
9460	        diff.phantom.len(),
9461	        diff.lost.len(),
9462	    ));
9463	    wl.live = fresh.live;
9464	    // the map now covers [base, replay_tip]: keep the view's claimed base
9465	    // consistent with what was actually replayed (same as the RPC verb)

===== EXCERPT: src/node_embedded.rs lines 9520-9600 — stake_tick — call sites: latch plumbed + unresolvable rate-limit =====
9520	    // stale flag stayed stuck — for exactly as long as the wallet could not
9521	    // stake anyway. Ownership evidence needs no secrets: resolve first
9522	    // (ONE SHARED KERNEL-EVIDENCE PATH — the exact resolution validate_block
9523	    // reads below; phantom coins excluded+flagged, unresolvable counted),
9524	    // determine the impairment, THEN apply the gates.
9525	    let (candidates, _no_key, _unloc, _nonstd) = wl.candidates();
9526	    stake_st.coins_considered = candidates.len() as u64;
9527	    let (mut candidates_rel, mut stale_coins, mut unresolvable_coins) =
9528	        resolve_stake_evidence(stg, candidates);
9529	    note_stale_evidence(stg, &stale_coins, stake_st.autoheal_fault_episode.as_deref());
9530	    if !stale_coins.is_empty() && autoheal_stale_live_view(stg, wl, store, stake_st) {
9531	        // item 27 §16-RACED-MINT-AUTOHEAL: the rebuild ran — re-resolve with
9532	        // the fresh view so THIS tick stakes on the restored coins instead of
9533	        // waiting a tick. A cleared evidence set clears the impairment (the
9534	        // next stake tick's CLEARED line stays silent — nothing to clear
9535	        // twice); a still-contradicted one keeps the impairment live for the
9536	        // next tick's determination.
9537	        let (c2, _nk, _ul, _ns) = wl.candidates();
9538	        let (c2_rel, stale2, unres2) = resolve_stake_evidence(stg, c2);
9539	        if stale2.is_empty() {
9540	            log(
9541	                "stake: AUTOHEAL — wallet evidence matches the chain again, impairment cleared in-process",
9542	            );
9543	            stg.staking_impaired = None;
9544	        }
9545	        candidates_rel = c2_rel;
9546	        stale_coins = stale2;
9547	        unresolvable_coins = unres2;
9548	    }
9549	    if wl.locked {
9550	        // Staking needs the private keys. An encrypted wallet that has not been
9551	        // unlocked cannot mint — say so once, then stay quiet. Evidence
9552	        // resolution above already ran: the impairment flag is live (or was
9553	        // just cleared) even while locked.
9554	        if !stake_st.locked_logged {
9555	            log("stake: wallet is LOCKED — no staking until walletpassphrase");
9556	            stake_st.locked_logged = true;
9557	        }
9558	        stake_st.coins_considered = 0;
9559	        return Ok(false);
9560	    }
9561	    stake_st.locked_logged = false;
9562	    let balance = wl.balance();
9563	    if balance <= tri7_core::stake::reserve_balance() {
9564	        stake_st.searches += 1;
9565	        return Ok(false);
9566	    }
9567	    let n_bits = get_next_target_required(&stg.rel_index(), true);
9568	    let median_time_past = stg
9569	        .median_time_past(tip_height)
9570	        .ok_or("median time past unavailable")?;
9571	    let started = Instant::now();
9572	    let fees = pool.total_fees();
9573	    if unresolvable_coins > 0 {
9574	        // item 37 STALE-RESOLVER-OFFBYONE: out-of-horizon coins are a
9575	        // PERMANENT, growing set (every reward UTXO crosses the WINDOW_ENTRIES
9576	        // edge eventually and then lives below base forever) — log at the
9577	        // house cadence (first tick + every 40th ≈10 min at the 15s stake
9578	        // tick), not every pass.
9579	        stake_st.unresolvable_ticks += 1;
9580	        if stake_st.unresolvable_ticks == 1 || stake_st.unresolvable_ticks % 40 == 0 {
9581	            log(&format!(
9582	                "stake: {} candidate coin(s) outside the chain kernel coverage (base {} / coverage floor window-rel {}) — skipped (the validator could not resolve their prevouts either; a below-floor coin is a kernel-coverage fact, NOT wallet corruption — repairwallet cannot fix it; item 24 boot backfill lifts the floor to 0)",
9583	                unresolvable_coins, stg.base, stg.utxo_floor_rel
9584	            ));
9585	        }
9586	    } else {
9587	        stake_st.unresolvable_ticks = 0;
9588	    }
9589	    let (plan, rep) = tri7_core::stake::create_coinstake(
9590	        &stg.rel_index(),
9591	        &candidates_rel,
9592	        n_bits,
9593	        fees,
9594	        now,
9595	        tri7_core::stake::MAX_STAKE_SEARCH_INTERVAL,
9596	        balance,
9597	        tip_height as usize + 1,
9598	    )?;
9599	    stake_st.attempts += rep.attempts;
9600	    stake_st.coins_meeting_min_age = rep.coins_meeting_min_age;

===== EXCERPT: src/node_embedded.rs lines 24631-24650 — item 37 test block start =====
24631	
24632	    /// THE DNS2 2026-11-21 INCIDENT REPLAY AT THE EXACT BOUNDARY. A coin
24633	    /// whose containing block sits at conf == WINDOW_ENTRIES (the window's
24634	    /// FAR EDGE — live first fire: tip h2217051, coin at h2212451 = base − 1)
24635	    /// has NO slot in the slid window: rebase must DROP its meta, never
24636	    /// clamp it onto entries[0] (the block AT the new base — the H+1
24637	    /// evidence read that produced the false facts-mismatch: resolver
24638	    /// claimed chain_block_time 1794744738 = h2212452's time while the
24639	    /// wallet's 1794744458 matched h2212451 EXACTLY). Post-fix the edge
24640	    /// coin classifies unresolvable (item 24, kernel-coverage fact) and
24641	    /// flags NOTHING, while a coin minted at the new base still resolves
24642	    /// through the chain-evidence read AT its claimed height (H-vs-H+1 pin).
24643	    #[test]
24644	    fn window_edge_coin_at_exactly_window_entries_conf_is_not_flagged() {
24645	        // synthetic_chain(0, WINDOW_ENTRIES): WINDOW_ENTRIES+1 entries →
24646	        // trim() slides exactly one (base 0 → 1) — the live first-fire shape.
24647	        let mut stg = synthetic_chain(0, WINDOW_ENTRIES as u64);
24648	        assert_eq!(stg.entries.len(), WINDOW_ENTRIES + 1);
24649	        let (secret, pubkey) = tri7_core::spend::synthetic_key("window-edge-4600");
24650	        let pubkey = pubkey.to_vec();

===== NEW TEST: window_edge_coin_at_exactly_window_entries_conf_is_not_flagged (src/node_embedded.rs, starts line 24643) =====
24643	    #[test]
24644	    fn window_edge_coin_at_exactly_window_entries_conf_is_not_flagged() {
24645	        // synthetic_chain(0, WINDOW_ENTRIES): WINDOW_ENTRIES+1 entries →
24646	        // trim() slides exactly one (base 0 → 1) — the live first-fire shape.
24647	        let mut stg = synthetic_chain(0, WINDOW_ENTRIES as u64);
24648	        assert_eq!(stg.entries.len(), WINDOW_ENTRIES + 1);
24649	        let (secret, pubkey) = tri7_core::spend::synthetic_key("window-edge-4600");
24650	        let pubkey = pubkey.to_vec();
24651	        // synthetic entries: time = 1.7e9 + h*600. The edge coin minted at
24652	        // abs 0 (conf == WINDOW_ENTRIES after the slide) with CORRECT wallet
24653	        // facts; a healthy coin minted at abs 1 (the new base).
24654	        let edge_time = 1_700_000_000u32;
24655	        let base_time = 1_700_000_000u32 + 600;
24656	        stg.utxo.map.insert(
24657	            ([0x31u8; 32], 0),
24658	            crate::kernel::UtxoMeta {
24659	                height: 0,
24660	                tx_offset: 181,
24661	                tx_time: edge_time,
24662	                value: 50 * tri7_core::consensus::COIN,
24663	            },
24664	        );
24665	        stg.utxo.map.insert(
24666	            ([0x32u8; 32], 0),
24667	            crate::kernel::UtxoMeta {
24668	                height: 1,
24669	                tx_offset: 181,
24670	                tx_time: base_time,
24671	                value: 50 * tri7_core::consensus::COIN,
24672	            },
24673	        );
24674	        stg.trim(); // the slide: drain 1 entry, base 0 → 1, rebase
24675	        assert_eq!(stg.base, 1);
24676	        assert!(
24677	            !stg.utxo.map.contains_key(&([0x31u8; 32], 0)),
24678	            "the edge coin's meta must be DROPPED, not clamped to slot 0"
24679	        );
24680	        assert!(stg.utxo.map.contains_key(&([0x32u8; 32], 0)));
24681	        let mk = |txid: [u8; 32], block_height: usize, t: u32| {
24682	            tri7_core::stake::StakeCandidate {
24683	                txid,
24684	                n: 0,
24685	                value: 50 * tri7_core::consensus::COIN,
24686	                script_pubkey: tri7_core::spend::script_pubkey_p2pk(&pubkey).unwrap(),
24687	                block_height,
24688	                block_time: t,
24689	                tx_time: t,
24690	                tx_offset: 181,
24691	                secret,
24692	                pubkey: pubkey.clone(),
24693	            }
24694	        };
24695	        let cands = vec![
24696	            mk([0x31u8; 32], 0, edge_time), // conf == WINDOW_ENTRIES
24697	            mk([0x32u8; 32], 1, base_time), // the new base, conf WINDOW_ENTRIES-1
24698	        ];
24699	        let (resolved, stale, unres) = resolve_stake_evidence(&stg, cands);
24700	        assert!(
24701	            stale.is_empty(),
24702	            "the false H+1 evidence read must NOT flag: got {stale:?}"
24703	        );
24704	        assert_eq!(
24705	            unres, 1,
24706	            "the edge coin leaves the candidate set as unresolvable (item 24)"
24707	        );
24708	        assert_eq!(resolved.len(), 1);
24709	        assert_eq!(resolved[0].txid, [0x32u8; 32]);
24710	        assert_eq!(resolved[0].block_height, 0, "survivor resolved window-relative");
24711	        assert_eq!(
24712	            resolved[0].block_time, base_time,
24713	            "chain evidence read AT the claimed height, not H+1"
24714	        );
24715	    }
24716	
24717	    /// item 37 (b): a coin whose containing block is PAST the evidence
24718	    /// horizon (conf > WINDOW_ENTRIES, several slides back) is counted
24719	    /// unresolvable on every pass and NEVER re-judged into the stale set —
24720	    /// the false flag must not persist past the edge the way the live DNS2
24721	    /// flag did (still firing at conf 4642, coins:2, no CLEARED ever).

===== NEW TEST: coin_past_the_horizon_is_never_rejudged_as_stale (src/node_embedded.rs, starts line 24722) =====
24722	    #[test]
24723	    fn coin_past_the_horizon_is_never_rejudged_as_stale() {
24724	        let mut stg = synthetic_chain(0, (WINDOW_ENTRIES + 5) as u64);
24725	        let (secret, pubkey) = tri7_core::spend::synthetic_key("past-horizon");
24726	        let pubkey = pubkey.to_vec();
24727	        let old_time = 1_700_000_000u32; // h0's time; the wallet claims it correctly
24728	        stg.utxo.map.insert(
24729	            ([0x41u8; 32], 0),
24730	            crate::kernel::UtxoMeta {
24731	                height: 0,
24732	                tx_offset: 181,
24733	                tx_time: old_time,
24734	                value: 25 * tri7_core::consensus::COIN,
24735	            },
24736	        );
24737	        stg.trim(); // excess 6 → base 6; the coin (abs 0) is 6 slides deep
24738	        assert_eq!(stg.base, 6);
24739	        assert!(!stg.utxo.map.contains_key(&([0x41u8; 32], 0)));
24740	        let cand = tri7_core::stake::StakeCandidate {
24741	            txid: [0x41u8; 32],
24742	            n: 0,
24743	            value: 25 * tri7_core::consensus::COIN,
24744	            script_pubkey: tri7_core::spend::script_pubkey_p2pk(&pubkey).unwrap(),
24745	            block_height: 0,
24746	            block_time: old_time,
24747	            tx_time: old_time,
24748	            tx_offset: 181,
24749	            secret,
24750	            pubkey,
24751	        };
24752	        for pass in 1..=3u8 {
24753	            let (resolved, stale, unres) = resolve_stake_evidence(&stg, vec![cand.clone()]);
24754	            assert!(
24755	                stale.is_empty(),
24756	                "pass {pass}: out-of-horizon coins are never re-judged as stale"
24757	            );
24758	            assert_eq!(
24759	                (resolved.len(), unres),
24760	                (0, 1),
24761	                "pass {pass}: counted+skipped (item 24), never a silent drop"
24762	            );
24763	        }
24764	    }
24765	
24766	    /// item 37 (c): a rebuild that finds ZERO owned-output mismatches while
24767	    /// the stale flag still fires is a RESOLVER fault — the fault line fires,
24768	    /// NO attempt burns, nothing swaps (the view was already correct), the
24769	    /// episode retires (a later re-log tick fires no further rebuild), and a
24770	    /// fresh episode re-arms via autoheal_due. Live DNS2: three silent no-op
24771	    /// rebuilds burned the full item-27 episode cap feeding an infinite
24772	    /// false flag.

===== NEW TEST: autoheal_rebuild_clean_latches_the_resolver_fault_and_burns_no_attempts (src/node_embedded.rs, starts line 24773) =====
24773	    #[test]
24774	    fn autoheal_rebuild_clean_latches_the_resolver_fault_and_burns_no_attempts() {
24775	        // the map-coverage fault shape: the wallet holds the honest coin with
24776	        // CORRECT chain-slot facts but stg.utxo lost the row — the resolver's
24777	        // absent branch flags it, the replay rebuilds the SAME view, diff 0.
24778	        let (secret, pubkey) = tri7_core::spend::synthetic_key("resolver-fault");
24779	        let _ = secret;
24780	        let pubkey = pubkey.to_vec();
24781	        let dir = std::env::temp_dir().join(format!(
24782	            "tri7-item37-{}-{}",
24783	            std::process::id(),
24784	            now_secs()
24785	        ));
24786	        std::fs::create_dir_all(&dir).unwrap();
24787	        let store = autoheal_store_with_honest_coin(
24788	            &dir,
24789	            &pubkey,
24790	            50 * tri7_core::consensus::COIN,
24791	        );
24792	        let wallet_keys = wallet_live_with(&[pubkey.clone()], vec![]).wallet;
24793	        let fresh0 = WalletLive::seed(&store, wallet_keys, 0, 4).unwrap();
24794	        let honest_key = *fresh0
24795	            .live
24796	            .keys()
24797	            .find(|k| fresh0.owns(&fresh0.live[k]))
24798	            .expect("honest coin in the replay");
24799	        let mut wl = wallet_live_with(
24800	            &[pubkey],
24801	            vec![(honest_key, fresh0.live[&honest_key].clone())],
24802	        );
24803	        wl.base = 0;
24804	        let mut stg = synthetic_chain(0, 4);
24805	        // stg.utxo stays EMPTY — the claimed h2 is inside the window and at/
24806	        // above the floor, so the absent branch fires: resolver fault, the
24807	        // wallet is right.
24808	        let (cands, _nk, _ul, _ns) = wl.candidates();
24809	        let (_rel, stale, _unres) = resolve_stake_evidence(&stg, cands);
24810	        assert_eq!(stale.len(), 1);
24811	        assert!(stale[0].absent, "the map-coverage fault shape is the absent class");
24812	        note_stale_evidence(&mut stg, &stale, None);
24813	        let first_utc = stg.staking_impaired.as_ref().unwrap().first_utc.clone();
24814	        let mut stake_st = gate_stake_state();
24815	        // first re-log tick: the rebuild runs, finds ZERO mismatches → latch
24816	        assert!(
24817	            !autoheal_stale_live_view(&mut stg, &mut wl, &store, &mut stake_st),
24818	            "nothing to swap — the view was already correct"
24819	        );
24820	        assert_eq!(
24821	            stake_st.autoheal_attempts, 0,
24822	            "rebuild-clean burns NO attempt budget"
24823	        );
24824	        assert_eq!(
24825	            stake_st.autoheal_fault_episode.as_deref(),
24826	            Some(first_utc.as_str()),
24827	            "the episode is latched as a resolver fault"
24828	        );
24829	        // later re-log tick: the latch retires the episode — no rebuild runs
24830	        stg.staking_impaired.as_mut().unwrap().ticks = 40;
24831	        assert!(!autoheal_stale_live_view(
24832	            &mut stg,
24833	            &mut wl,
24834	            &store,
24835	            &mut stake_st
24836	        ));
24837	        assert_eq!(stake_st.autoheal_attempts, 0);
24838	        // a fresh episode (new first_utc) re-arms: the latch clears
24839	        let impaired_now = {
24840	            let i = stg.staking_impaired.as_mut().unwrap();
24841	            i.first_utc = "2026-11-22T00:00:00Z".into();
24842	            i.ticks = 1;
24843	            i.clone()
24844	        };
24845	        assert!(autoheal_due(&impaired_now, &mut stake_st));
24846	        assert_eq!(stake_st.autoheal_fault_episode, None, "fresh episode re-arms");
24847	    }
24848	
24849	    /// item 37: the resolver-fault latch drives the STALE line's remedy —
24850	    /// the re-log must NOT tell the operator to run repairwallet when this
24851	    /// episode's rebuild already proved the wallet clean (the no-op advice
24852	    /// was DNS2's false alarm remedy). Observable state: the fault latch is
24853	    /// keyed by the episode's first_utc, and note_stale_evidence only swaps
24854	    /// the remedy text for the latched episode — assert the latch keying
24855	    /// both note_stale_evidence consumers share.

===== NEW TEST: resolver_fault_remedy_text_is_keyed_to_the_latched_episode (src/node_embedded.rs, starts line 24856) =====
24856	    #[test]
24857	    fn resolver_fault_remedy_text_is_keyed_to_the_latched_episode() {
24858	        let mut stg = synthetic_chain(0, 4);
24859	        let mk = |first: &str| StaleLiveView {
24860	            txid: "fault".into(),
24861	            n: 0,
24862	            wallet_height: 2,
24863	            wallet_block_time: 1,
24864	            chain_block_time: 2,
24865	            wallet_tx_time: 1,
24866	            chain_tx_time: 1,
24867	            absent: true,
24868	            coins: 1,
24869	            first_utc: first.into(),
24870	            ticks: 0,
24871	        };
24872	        note_stale_evidence(&mut stg, &[mk("e1")], Some("e1"));
24873	        assert_eq!(stg.staking_impaired.as_ref().unwrap().first_utc, "e1");
24874	        // the latched episode keeps flagging (ticks climb) — the remedy text
24875	        // selector stays on the fault arm for THIS episode only
24876	        note_stale_evidence(&mut stg, &[mk("e1")], Some("e1"));
24877	        assert_eq!(stg.staking_impaired.as_ref().unwrap().ticks, 2);
24878	        // an UNLATCHED episode (no fault recorded) takes the repairwallet arm
24879	        stg.staking_impaired = None;
24880	        note_stale_evidence(&mut stg, &[mk("e2")], None);
24881	        assert_eq!(stg.staking_impaired.as_ref().unwrap().first_utc, "e2");
24882	        // a fault latched for a DIFFERENT episode does not swap this one
24883	        stg.staking_impaired = None;
24884	        note_stale_evidence(&mut stg, &[mk("e3")], Some("e1"));
24885	        assert_eq!(stg.staking_impaired.as_ref().unwrap().first_utc, "e3");
24886	    }
