//! Fails the build at configure time, naming the missing system dependency,
//! instead of letting a missing `libxdo` surface as a bare linker error at
//! the very end of the `familiar-ai-daemon` build. See `src/sysdeps.rs`.
//!
//! `familiar-ai-tray` is an ordinary workspace member, so `cargo build
//! --workspace` / `cargo test --workspace` compile it even when no consumer
//! has asked for the tray. The abort therefore only fires when the
//! `enforce-native-deps` feature is enabled — set by `familiar-ai-daemon`'s
//! `tray` feature — so a plain workspace build with the tray disabled gets a
//! `cargo:warning` instead of a hard failure.

#[path = "src/sysdeps.rs"]
mod sysdeps;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_ENFORCE_NATIVE_DEPS");
    // Without this, cargo fingerprints this build script only on the inputs
    // declared above and never re-runs it for changes to this var alone —
    // on a host that has libxdo, a prior successful build's cached fingerprint
    // would make a later run with this var forced empty (see
    // tray_feature_build.rs's missing-dependency regression) succeed instead
    // of re-detecting the "removed" dependency.
    println!(
        "cargo:rerun-if-env-changed={}",
        sysdeps::SEARCH_DIRS_OVERRIDE_ENV
    );

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }

    let Some(missing) = sysdeps::find_missing(&sysdeps::default_search_dirs()) else {
        return;
    };

    if std::env::var_os("CARGO_FEATURE_ENFORCE_NATIVE_DEPS").is_some() {
        panic!("\n\n{}\n", missing.diagnostic());
    }

    for line in missing.diagnostic().lines() {
        println!("cargo:warning={line}");
    }
}
