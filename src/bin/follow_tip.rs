//! follow_tip — thin shim: the node lives in the library now
//! (tri7_core::node_embedded) so the GUI can run it in-process. This bin
//! keeps the external-daemon workflow (fleet nodes) working unchanged.
fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = tri7_core::node_embedded::main_args(&argv);
    std::process::exit(code);
}
