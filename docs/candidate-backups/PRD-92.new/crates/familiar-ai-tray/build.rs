//! Fails the build at configure time, naming the missing system dependency,
//! instead of letting a missing `libxdo` surface as a bare linker error at
//! the very end of the `familiar-ai-daemon` build. See `src/sysdeps.rs`.

#[path = "src/sysdeps.rs"]
mod sysdeps;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        if let Some(missing) = sysdeps::find_missing(&sysdeps::default_search_dirs()) {
            panic!("\n\n{}\n", missing.diagnostic());
        }
    }
}
