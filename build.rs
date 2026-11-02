use std::path::Path;

fn main() {
    let vendor = Path::new("vendor");
    // Legacy sphlib sources, copied verbatim from triangles_v5/src — the exact
    // C implementations the live chain was built with.
    let sph_sources = [
        "blake.c", "bmw.c", "groestl.c", "skein.c", "jh.c", "keccak.c",
        "luffa.c", "cubehash.c", "shavite.c", "simd.c", "echo.c",
        "hamsi.c", "fugue.c",
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
