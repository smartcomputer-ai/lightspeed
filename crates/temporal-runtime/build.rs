fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();
    if target_os == "macos" && profile == "debug" {
        // Apple's compact-unwind table is limited to 16 MiB. The debug
        // Temporal runtime and live-test binaries exceed that limit, so ask the
        // linker to retain the equivalent DWARF unwind data directly instead
        // of attempting (and warning about) a compact table it cannot encode.
        println!("cargo:rustc-link-arg=-Wl,-no_compact_unwind");
    }
}
