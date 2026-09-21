# Familiar desktop

`familiar-ai-desktop` is the Tauri graphical client for the independently
supervised, headless `familiar-ai-daemon`. It owns the tray and application
windows but has no direct database, configuration-file, credential, shell, or
provider-network authority. All queries and mutations use the daemon's typed,
same-user local protocol.

## Build and install

Ordinary development requires Rust and the platform WebView toolchain; it does
not require Docker or a browser.

On macOS, install Xcode Command Line Tools. Build a signable application bundle
with Tauri CLI 2:

```sh
cargo install tauri-cli --version '^2' --locked
cd crates/familiar-ai-desktop
cargo tauri build --bundles app
```

On Debian/Ubuntu Linux, install the non-Docker runtime and build prerequisites:

```sh
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev patchelf
cd crates/familiar-ai-desktop
cargo tauri build --bundles deb,appimage
```

The generated Debian package declares `libwebkit2gtk-4.1-0` and
`libayatana-appindicator3-1` as runtime dependencies. Missing packaging tools
cause the Tauri build to fail with their native diagnostic; they do not disable
the desktop or select a browser fallback.

Install independent per-user services after putting `familiar-ai`,
`familiar-ai-daemon`, and `familiar-ai-desktop` in the same directory:

```sh
familiar-ai ops desktop install
familiar-ai ops desktop status
```

`Quit Desktop` affects only the UI process. `Stop Familiar` is a separately
confirmed daemon mutation. Uninstall preserves configuration and durable
history:

```sh
familiar-ai ops desktop uninstall
```
