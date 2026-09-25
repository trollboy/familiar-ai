#[path = "src/sysdeps.rs"]
mod sysdeps;

fn main() {
    println!("cargo:rerun-if-env-changed={}", sysdeps::PKG_CONFIG_ENV);

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }

    let program = std::env::var_os(sysdeps::PKG_CONFIG_ENV).unwrap_or_else(|| "pkg-config".into());
    if let Err(diagnostic) = sysdeps::probe_linux_gtk(&program) {
        panic!("{diagnostic}");
    }
}
