use std::path::Path;

fn main() {
    let vendor = Path::new("vendor");
    embed_windows_resources();
    // Legacy sphlib sources, copied verbatim from triangles_v5/src — the exact
    // C implementations the live chain was built with.
    let sph_sources = [
        "blake.c",
        "bmw.c",
        "groestl.c",
        "skein.c",
        "jh.c",
        "keccak.c",
        "luffa.c",
        "cubehash.c",
        "shavite.c",
        "simd.c",
        "echo.c",
        "hamsi.c",
        "fugue.c",
    ];
    // NOTE: aes_helper.c and hamsi_helper.c are intentionally NOT compiled as
    // standalone TUs — the algorithm sources #include them directly.
    let mut build = cc::Build::new();
    build.include(vendor);
    for src in sph_sources {
        build.file(vendor.join(src));
    }
    build.file(vendor.join("hash9.c"));
    build.flag_if_supported("-O2");
    build.compile("sphlib");

    println!("cargo:rerun-if-changed=vendor");
}

// ---- Windows exe branding: triangle icon + version metadata (Sami: proper
// branding — the triangle symbol IS the wallet icon, and the exe carries real
// FileVersion/ProductVersion so the updater and Explorer show it) ----
// NOTE: not #[cfg(windows)] — build.rs compiles for the HOST; the target is
// checked at runtime via CARGO_CFG_WINDOWS, and the winresource dep is
// target-gated in Cargo.toml.
fn embed_windows_resources() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let ico = "deploy/windows/assets/triangles.ico";
    if !Path::new(ico).exists() {
        println!("cargo:warning=icon missing, skipping windows resources: {}", ico);
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico);
    res.set("FileDescription", "Cryptographic Triangles Wallet");
    res.set("ProductName", "Cryptographic Triangles");
    res.set("OriginalFilename", "tri7_wallet_gui.exe");
    if let Ok(v) = std::env::var("CARGO_PKG_VERSION") {
        res.set("FileVersion", &format!("{}.0.0", v));
        res.set("ProductVersion", &v);
    }
    if let Err(e) = res.compile() {
        println!("cargo:warning=windows resource embed failed: {}", e);
    }
}
