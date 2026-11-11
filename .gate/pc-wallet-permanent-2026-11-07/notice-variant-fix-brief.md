# One-word fix to the graded balance change

CI failed on all three GUI jobs with:
  error[E0599]: no variant, associated function, or constant named `Warn`
                found for enum `Notice` in the current scope

The theme's notice kinds are Info / Success / Warning / Error
(src/gui/theme.rs:817). The graded change used `th::Notice::Warn`, which
does not exist. The fix uses the real variant name, `Warning`.

Scope: one line in src/gui/app.rs (the partial-balance notice).
`cargo build --release --bin tri7_wallet_gui`: clean.
`cargo test --release --lib`: 507 passed, 0 failed, 1 ignored.
