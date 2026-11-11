# Judge brief: tri7 lane-i18n round 2 — GUI i18n phase 1 (12 languages), UB removed

## What this artifact is
Phase-1 internationalization for the tri7 egui GUI: Language enum with 12
variants (en + es/de/fr/pt/ru/zh/ja/tr/it/nl/pl), tr() with English-identity +
fallback-to-key semantics (never blank), static in-binary tables, 40 phase-1
keys × 11 languages = 440 entries, a REAL Options language ComboBox persisting
to options.json, and wiring across sidebar nav + TOOLS, 11 page headers,
Overview balance cards, Transactions columns/filters/placeholders, Export CSV,
Options tabs, status chips.

## ROUND-1 F blockers — both addressed (parent-verified)
1. `unsafe { std::mem::transmute::<&str, &'static str> }` in english_identity()
   (lifetime forgery = UB) → REMOVED: tr() is now safe, returns owned String,
   match arms return owned Strings; the helper is gone (grep: zero `unsafe`
   anywhere in i18n.rs; the only match is the doc-comment line 136 recording
   the round-1 ruling).
2. TOOLS header unwired → WIRED (i18n::tr(lang, "TOOLS") at app.rs:491).

## ROUND-2 parent-verification catches (this session, before grading)
The wiring-check test (rewritten round 1 to collect unwired keys) still
grepped for the exact variable name `lang` and missed real wiring:
- 8 Overview balance-card strings (Spendable:/Stake:/Unconfirmed:/Immature:
  labels + 4 tooltips) were NOT wrapped in tr() at all (the lane's round-1
  report overclaimed) → parent wrapped all 8 with i18n::tr(lang_row, ...).
- spend_tip was bound to a bare &str literal → binding now carries the key
  literal through tr().
- The wiring check itself was made variable-name-agnostic: it scans every
  i18n::tr( call site and extracts the first string-literal argument into a
  key set, then asserts PHASE1_KEYS ⊆ wired ∪ screen-titles. (It caught the
  spend_tip case AFTER the parent's first wrap — the check now genuinely
  guards the contract.)

## Tests (+5 net vs base)
completeness (440 entries × exact key sets per language), fallback-to-key,
persistence through a REAL options.json file, wiring-check via call-site scan,
TOOLS assertion. Suite (lane worktree, private target-i18n):
**555 passed; 0 failed; 1 ignored** (base 553 + net new).

## Scope
Exactly: src/gui/i18n.rs (NEW, in bundle as i18n-full.rs), src/gui/app.rs,
src/gui/app_state.rs, src/gui/tests.rs (+287/-38 across the 3 modified).
No git state changes; no other lanes touched. Machine-drafted translations
are flagged in module docs for native-speaker review; Arabic deferred to
phase 2 (egui RTL partial).

## Evidence files
brief.md; i18n.diff (3 modified files); i18n-full.rs (the new module);
tests-i18n-r2.txt (suite tail, isolated target).

## ROUND 3 — judge blockers addressed (parent-executed fixes)
1. **tr() API truth**: tr() returns a BORROWED `&'a str` tied to the key's
   lifetime (static 'static table values coerce; English identity returns the
   key). Safe, zero `unsafe` — the round-2 brief's "owned String" claim was
   wrong and is withdrawn; the code never needed ownership.
2. **Double translation removed**: the balance Grid's row closure ALREADY
   translates both params (tr(lang_row, label), tr(lang_row, tip)) — the
   round-2 outer wraps translated twice (second lookup falls back to the
   already-translated text; wrong contract). REVERTED: raw keys flow into the
   closure and are translated exactly once.
3. **Compile-order defect fixed**: the round-2 spend_tip edit referenced
   lang_row before its declaration inside the closure — the lib suite never
   compiles the feature-gated app.rs (include_str! reads it as TEXT), so the
   suite stayed green on a non-compiling tree. The binding is back to the raw
   literal and — the load-bearing evidence change — tests-i18n-r3.txt now
   includes `cargo check --bin tri7_wallet_gui --features gui`, which COMPILES
   src/gui/app.rs.
4. **Wiring scanner bounded**: each extraction window now stops at the next
   i18n::tr( call (hard cap 400 chars), so dynamic calls like
   tr(lang_row, label) can never absorb a literal from later source text.
   The 8 balance keys are covered by an explicit documented seam (they flow
   through the closure as params): keys must appear verbatim as literals in
   app.rs AND the closure must translate both params (asserted).
5. **Evidence regenerated from the exact reviewed tree**: full command,
   worktree/branch/base identity, git status, complete filtered test output,
   and the GUI-feature compile log — tests-i18n-r3.txt.

## ROUND 4 — the decisive blocker: app.rs had NEVER been compiled
The feature-gated check exposed 3 E0422 errors (sidebar + overview_balance
used `lang` with no binding — the round-1 wiring added tr() calls to two fns
without declaring `lang`). Both bindings added; GUI-feature check now
compiles clean (0 errors). Lib suite still 555/0/1 on the same tree.
Evidence: tests-i18n-r4.txt (commands, state identity, complete test tail,
GUI-feature error count + Finished line).
