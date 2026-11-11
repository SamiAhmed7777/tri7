# Two things Sami asked for, in one bundle

## 1. THE WALLET MUST SHOW THE WHOLE BALANCE

Sami's screenshots showed a wallet holding 5,291 TRI (the daemon's own
authoritative `getwalletinfo`: `balance 5291.900481`, `balance_height
2208050`, `balance_partial false`, 399 keys, 244 UTXOs) while the GUI's
Overview card showed 1,627.94 TRI and the Staking page showed 1,712.81.

THE CAUSE, MEASURED: the card sums `listunspent`. The daemon served
`listunspent` from the wallet LIVE VIEW, and the live view is seeded from
the chain-state WINDOW base - so on a node that resumed from a state file
it knows only the coins that arrived INSIDE the window. On this PC the
window starts at h2203458 and the wallet's coins are older than that, so
the live view held 7 outputs (the PC's own freshly minted stake outputs)
worth 16.29 TRI. The coins the wallet actually holds - 5,275 TRI of them -
were simply not in the list.

THE FIX (src/bin/follow_tip.rs, `LiveWalletProvider::utxos`): the wallet's
unspent set is now

    the replay SNAPSHOT (genesis..coverage)
      PLUS the live view's outputs NEWER than that coverage
      MINUS the snapshot coins that were SPENT after that coverage

The third clause matters and was added after the first review round: a coin
the snapshot lists can have been spent after the snapshot's coverage. The
live view applies every block from its window base onwards, so it SEES
that spend - but the coin is not in the live map (it predates the window),
so the removal was a no-op and the snapshot would keep listing a coin that
no longer exists. The live view therefore now records every prevout it
saw being spent, with the height of the spending block, and the merge
drops the matching snapshot row when that height is above the coverage.
A spend at or below the coverage is already reflected in the snapshot, so
it removes nothing.

The two ranges do not overlap - the live rows kept are strictly above the
coverage height and the snapshot ends at it - so no coin is counted twice.

THE GAP IS REJECTED, NOT IGNORED. The snapshot covers genesis..coverage;
the live view covers its window base..tip. When the live view's base is
ABOVE the snapshot's coverage there is a hole between them that NEITHER
source covers: a coin received in the hole would be missing (the wallet
would understate) and a coin spent in the hole would still be listed (the
wallet would overstate). Such a merge is not a wallet - it is two halves
of one with a piece missing - so it is REJECTED: the snapshot alone is
served, which is a consistent set as of its own coverage height, and the
RPC's completeness rule marks the figure accordingly. The merge therefore
cannot silently overstate or understate the wallet.

Extracted as pure functions so the rule is testable without a node:
`live_rows_for()` and `merge_whole_wallet_utxos()`.

TESTS (src/bin/follow_tip.rs, 53 passing in the daemon's own suite):

    the_unspent_set_is_the_snapshot_plus_the_fresh_live_outputs
        snapshot {100, 200} at coverage 200, live {999 at h150, 333 at h250}
        -> 3 rows, 633 sats total; the live view's OLD output is not
           counted twice; the fresh one IS included
    a_snapshot_coin_spent_after_the_coverage_is_removed
        the snapshot holds two coins, one of them is spent at h260 (after
        the coverage h200) -> that coin is NOT listed; the total is 533
    a_spend_at_or_below_the_coverage_removes_nothing
        a spend at h150 with coverage h200 removes nothing - the snapshot
        is the authority for its own range
    a_gap_between_the_coverage_and_the_live_window_base_is_rejected
        the snapshot covers genesis..h100, the live window starts at h200
        -> the merge is refused; the snapshot alone is served
    the_merge_boundary_is_the_live_window_base_versus_the_coverage
        base BELOW the coverage -> merged exactly (2 rows)
        base EQUAL to the coverage -> merged exactly (2 rows)
        base ONE ABOVE the coverage -> rejected (the gap)
    with_no_snapshot_the_live_set_is_served_as_is
        a fresh wallet folder (no snapshot files) keeps the live set -
        and the live rows are returned REGARDLESS of the live window
        base, because with no snapshot there is nothing to merge with and
        no hole to worry about

## 2. MINIMIZE ON CLOSE, AND MINIMIZE TO THE SYSTEM TRAY

Sami's screenshot of Options -> Window showed both checkboxes DISABLED
with the tooltip "Not supported by this desktop shell".

They are now real:

  * "Minimize on close" - the window minimizes, the process AND the node
    keep running. This already worked; it was just not offered.
  * "Minimize to the tray instead of the taskbar" - the window is HIDDEN
    and the wallet lives in the system tray, with a context menu
    ("Show Triangles" / "Exit"). A left click on the icon brings the
    window back.

NEW: src/gui/tray.rs (154 lines, in this artifact) - a real tray icon
(tray-icon + muda, Windows target only) built from the wallet's own brand
mark, polled from the eframe update loop so no extra message loop is
needed.

THE ICON MUST EXIST BEFORE THE WINDOW IS HIDDEN INTO IT. Two rules make
that true:

  * `CloseDecision` gained a `Tray` variant, and `close_decision()` now
    takes `has_tray`. The rule is deliberately conservative: the wallet is
    NEVER hidden into a tray that does not exist, and the wallet NEVER
    dies when the operator asked for it to keep running. A tray was asked
    for but is not there -> the window minimizes to the taskbar instead.
  * Turning EITHER checkbox creates or destroys the icon RIGHT AWAY
    (`sync_tray_icon`), so the choice takes effect without a restart and
    the icon always matches the option. If the icon cannot be created the
    option is turned back off and the status log says so - the window is
    never hidden into nowhere.
  * At STARTUP the same rule applies: if the saved option asked for the
    tray but the icon cannot be created, the option is turned off before
    the first frame, so the state and the reality agree.

The Options Window tab now offers both checkboxes honestly: on a platform
with a tray they are editable, and where there is no tray the tooltip says
so ("This build has no system tray on this platform") instead of silently
doing nothing.

ABOUT THE RELATIONSHIP BETWEEN THE TWO CHECKBOXES: they are two places
to express ONE choice - "close the window, keep the wallet running".
Turning the tray OFF therefore leaves that choice on (the window goes back
to the taskbar); turning "minimize on close" OFF turns the tray off too,
because there would then be nothing left to keep the wallet alive.

TESTS (src/gui/tests.rs):

    the_window_tab_choices_persist_and_survive_a_reload
        both choices persist to options.json, survive a reload, and the
        close button keeps the wallet alive (CloseDecision::Tray)
    close_behaviour_flags_cover_every_combination
        the full truth table, now including the Tray variant and the
        fallback: (minimize, tray, has_tray) -> decision, including
        "tray wanted but no tray -> Minimize"
    a_missing_tray_falls_back_to_the_taskbar_instead_of_hiding
        with no tray the decision is never Tray, and the fallback is the
        taskbar so the wallet keeps running

## BUILD (reproducible logs in this artifact)

    cargo build --release --features gui --bin tri7_wallet_gui      -> clean
    cargo check --release --features gui --bin tri7_wallet_gui
        --target x86_64-pc-windows-gnu                             -> clean
        (the Windows-only tray path is compile-checked locally before CI)

    cargo test --release --lib                -> 514 passed; 0 failed; 1 ignored
    cargo test --release --bin follow_tip     ->  55 passed; 0 failed
    cargo test --release --features gui --lib -> 523 passed; 0 failed; 2 ignored

The tray dependency is `tray-icon`, added as a cfg(windows)-only optional
dependency so the headless Linux/macOS builds are unaffected.


## ONE MORE FACT THE SNAPSHOT NOW PUBLISHES

The completeness rule needs to know the tip a replay was CHASING, and
`end_height` cannot say it: `end_height` is where the walk is ALLOWED to
stop (0 = walk to EOF), while the target is where it was TRYING to get
to. They differ when the store's last body sits below the tip.

So `BalancesBuildOpts` gained `replay_target`:

    the daemon passes the tip it is chasing (Some(tip))
    the standalone balances tool passes the height it was told to reach

and the snapshot publishes it as `replay_target_height`. The RPC's rule
then compares the snapshot's coverage against THAT target: a replay that
stopped short of the tip it was chasing was capped by a hole in the
store, and its figure omits everything above the cap.

(The daemon's own snapshot is rebuilt with this field, so its published
coverage now carries the fact instead of leaving it unknown.)


## THE INTERVAL SEMANTICS, STATED EXACTLY

The snapshot covers the height range [0, coverage] - genesis up to and
including its coverage height. The live view covers [live_base, tip] -
its window base up to the tip.

The two ranges are contiguous - every height is covered by one of them -
when live_base <= coverage + 1. When live_base > coverage + 1 the
heights in (coverage, live_base) are covered by NEITHER: that is the
hole, and the merge is refused.

The boundary test uses exactly those values with coverage = 200:

    live_base = 100  (below)  -> merged exactly, 2 rows
    live_base = 200  (equal)  -> merged exactly, 2 rows
    live_base = 202  (above)  -> refused, 1 row (the snapshot alone)

and live_base = 201 is the last contiguous value.

## THE TRAY'S "Exit" MUST REALLY EXIT

The tray's "Exit" item asks the window to close. But the close button's
keep-running behaviour would SWALLOW that request and minimize the
wallet instead - so "Exit" would have done nothing. The app therefore
carries a `force_quit` flag: the tray sets it, and the close handler
checks it FIRST and lets the window close for real. The flag is consumed
so it does not linger.

TEST (src/gui/tests.rs):

    a_tray_exit_request_is_not_swallowed_by_the_keep_running_behaviour
        a plain close with the tray chosen would hide into the tray; the
        Exit request sets force_quit, which the close handler consumes so
        the window really closes

## THE TRAY ICON IS SQUARE

`Icon::from_rgba` is given a size x size square, so the source image must
be square too - a non-square image would be declared with the wrong byte
count. `brand_icon()` now checks the dimensions and scales the mark into
a square if it ever is not one, rather than mis-declaring it.
