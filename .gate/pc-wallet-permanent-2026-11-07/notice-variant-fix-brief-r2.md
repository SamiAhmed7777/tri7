# One-word fix to the graded balance change

CI failed on all three GUI jobs with:
  error[E0599]: no variant, associated function, or constant named `Warn`
                found for enum `Notice` in the current scope

The theme's notice kinds are Info / Success / Warning / Error. The graded
change used `th::Notice::Warn`, which does not exist. The fix uses the real
variant name, `Warning`.

## The evidence, from the tree itself

Base revision: 8f224c26b4e46d1a0c2ea7782824fb7022a39016 (source tree /root/tri7, branch master)

The enum, verbatim from src/gui/theme.rs:

```rust
);
        copied = ghost_button(ui, "copy").clicked();
    });
    copied
}

/// Semantic notice kinds.
pub enum Notice {
    Info,
    Success,
    Warning,
    Error,
}

impl Notice {
    fn color(&self) -> Color32 {
        match self {
            Notice::Info => INFO,
            Notice::Success => SUCCESS,
            Notice::Warning => WARNING,
            Notice::Error => ERROR,
        }
    }
}

/// `inline_notice` — bordered semantic result panel (never bare colored text).
```

The build, from this exact tree:

```
warning: `tri7-core` (bin "tri7_wallet_gui") generated 2 warnings
    Finished `release` profile [optimized] target(s) in 0.22s
```

The test suite, from this exact tree:

```
test result: ok. 507 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 14.21s
```

The diff is one line in src/gui/app.rs (the partial-balance notice).
