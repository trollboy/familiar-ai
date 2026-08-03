# PRD-001: Daemon Skeleton + Config + Logging

## Overview

Bootstrap the Familiar Rust project: Cargo workspace, background daemon process, configuration system, structured logging, and platform-aware path management. This is the foundation everything else builds on.

## Goals

- Runnable daemon binary that starts, logs, and shuts down cleanly
- Configuration file support (TOML) via `figment` with layered sources
- Platform-aware path abstraction (`AppPaths`) for XDG/macOS compliance
- Structured logging (stdout + file rotation)
- Signal handling (SIGTERM, SIGINT) for graceful shutdown
- Tokio async runtime as the execution foundation
- Docker development environment with test service
- CI-ready project structure with stub crates for future work

## Non-Goals

- No SQLite yet (PRD-002)
- No file watching (PRD-003)
- No systray (PRD-004)
- No MCP server logic (PRD-005) — but the crate exists as a stub
- No actual business logic

## Technical Requirements

### Project Structure

```
familiar/
├── Cargo.toml              (workspace root)
├── Cargo.lock
├── config/
│   └── default.toml        (default configuration)
├── crates/
│   ├── familiar-daemon/     (main daemon binary)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   ├── familiar-core/       (shared types, config, errors, paths)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs
│   │       ├── error.rs
│   │       └── paths.rs
│   ├── familiar-logging/    (tracing setup)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   ├── familiar-mcp/        (stub — MCP server, PRD-005)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   ├── familiar-storage/    (stub — SQLite, PRD-002)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   └── familiar-testutil/   (shared test helpers)
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs
├── docker-compose.yml
├── Dockerfile
├── .dockerignore
├── .gitignore
└── docs/
```

### Platform-Aware Paths (`familiar-core::paths`)

```rust
pub struct AppPaths {
    pub config_dir: PathBuf,    // Linux: $XDG_CONFIG_HOME/familiar | macOS: ~/Library/Application Support/Familiar
    pub data_dir: PathBuf,      // Linux: $XDG_DATA_HOME/familiar   | macOS: ~/Library/Application Support/Familiar
    pub state_dir: PathBuf,     // Linux: $XDG_STATE_HOME/familiar  | macOS: ~/Library/Application Support/Familiar
    pub runtime_dir: PathBuf,   // Linux: $XDG_RUNTIME_DIR/familiar | macOS: /tmp/familiar-$UID
    pub log_dir: PathBuf,       // Linux: $XDG_STATE_HOME/familiar/log | macOS: ~/Library/Logs/Familiar
    pub socket_path: PathBuf,   // runtime_dir/familiar.sock
    pub pid_path: PathBuf,      // state_dir/familiar.pid
}
```

Linux defaults (with XDG fallbacks):
- config_dir: `$XDG_CONFIG_HOME/familiar` → `~/.config/familiar`
- data_dir: `$XDG_DATA_HOME/familiar` → `~/.local/share/familiar`
- state_dir: `$XDG_STATE_HOME/familiar` → `~/.local/state/familiar`
- runtime_dir: `$XDG_RUNTIME_DIR/familiar` → `/tmp/familiar-$UID`
- log_dir: `$XDG_STATE_HOME/familiar/log`
- pid_path: `$XDG_STATE_HOME/familiar/familiar.pid`
- socket_path: `$XDG_RUNTIME_DIR/familiar/familiar.sock`

macOS defaults:
- config_dir: `~/Library/Application Support/Familiar`
- data_dir: `~/Library/Application Support/Familiar`
- state_dir: `~/Library/Application Support/Familiar`
- runtime_dir: `/tmp/familiar-$UID`
- log_dir: `~/Library/Logs/Familiar`
- pid_path: `~/Library/Application Support/Familiar/familiar.pid`
- socket_path: `/tmp/familiar-$UID/familiar.sock`

`AppPaths::new()` auto-detects platform. Directories are created on first use, not at construction time.

### Configuration (`familiar-core::config`)

Config loaded via `figment` from (in priority order):
1. CLI flags (via figment's `Serialized` provider from clap values)
2. Environment variables (`FAMILIAR_` prefix, `__` as separator)
3. Config file (`{config_dir}/config.toml`)
4. Built-in defaults (embedded in code)

Config struct:

```rust
pub struct Config {
    pub daemon: DaemonConfig,
    pub logging: LoggingConfig,
    pub llm: LlmConfig,
}

pub struct DaemonConfig {
    // paths derived from AppPaths unless overridden
    pub pid_file: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
    pub heartbeat_interval_secs: u64,  // default: 60
}

pub struct LoggingConfig {
    pub level: String,            // default: "info"
    pub file: Option<PathBuf>,    // default: None (stdout only)
    pub format: LogFormat,        // default: LogFormat::Pretty
}

pub enum LogFormat {
    Pretty,
    Json,
}

pub struct LlmConfig {
    pub enabled: bool,            // default: false
    pub model_path: Option<PathBuf>,
}
```

When `pid_file` or `socket_path` are `None`, the daemon uses `AppPaths` defaults.

### Daemon (`familiar-daemon`)

- Starts tokio runtime
- Constructs `AppPaths`, ensures directories exist
- Loads config via figment
- Initializes tracing subscriber (via `familiar-logging`)
- Writes PID file
- Registers signal handlers (SIGTERM, SIGINT)
- Runs main event loop (initially just a heartbeat log every 60s)
- Cleans up PID file on shutdown
- Exit codes: 0 = clean shutdown, 1 = error

CLI flags (use `clap`):
- `--config <path>` — override config file path
- `--log-level <level>` — override log level
- `--foreground` — run in foreground (default; daemonization deferred)

### Logging (`familiar-logging`)

Use `tracing` + `tracing-subscriber`:
- Structured fields (timestamp, level, target, message)
- Optional JSON output for production
- File appender with rotation (via `tracing-appender`)
- Filter by level per-crate
- Init function takes `LoggingConfig` + optional `log_dir` from `AppPaths`

### Error Handling

Use `thiserror` for error types in `familiar-core`:

```rust
pub enum FamiliarError {
    Config(String),
    Io(std::io::Error),
    AlreadyRunning,
    Shutdown(String),
}
```

### Version Info (`familiar-core`)

```rust
pub struct VersionInfo {
    pub version: String,          // from Cargo.toml
    pub git_sha: Option<String>,  // from build script
    pub build_date: Option<String>,
    pub rust_version: Option<String>,
}
```

Populated at compile time via `build.rs` or environment variables. Logged at startup. Available for tray About dialog, diagnostics, and future /health endpoint.

### App Status (`familiar-core`)

```rust
pub struct AppStatus {
    pub startup_time: DateTime<Utc>,
    pub active_projects: usize,
    pub local_llm_enabled: bool,
    pub mcp_enabled: bool,
    pub last_heartbeat: DateTime<Utc>,
}
```

Updated by the daemon heartbeat loop. Initially `active_projects: 0`, `mcp_enabled: false`. Foundation for tray tooltip, /health, /status, dashboard, and metrics.

### Stub Crates

`familiar-mcp`: Empty lib with a doc comment explaining future purpose. Re-exports nothing.

`familiar-storage`: Empty lib with a doc comment explaining future purpose. Re-exports nothing.

### Docker

`Dockerfile`:
- Multi-stage build (builder + runtime)
- Rust 1.78+ base image
- Cargo chef for layer caching

`docker-compose.yml`:
- `app` service: runs the daemon
- `test` service: runs `cargo test` with coverage via `cargo-llvm-cov`
- Volume mounts for data and config directories
- Config volume mount

### Tests

- `AppPaths`: correct paths on Linux, respects XDG env vars, fallback behavior
- Config loading: defaults, file override, env override, CLI override, figment layering
- Signal handling: daemon shuts down cleanly on SIGTERM
- PID file: created on start, removed on shutdown, detects already-running
- Error types: proper display/debug formatting
- Logging: init with pretty format, init with JSON format
- Integration test: start daemon, verify heartbeat log, send SIGTERM, verify clean exit

## Acceptance Criteria

1. `cargo build` succeeds with no warnings
2. `cargo test` passes all tests
3. `docker compose run test` passes all tests with ≥85% coverage
4. Daemon starts, logs a startup message, and shuts down on SIGTERM
5. Config loads from file, env vars, and CLI flags with correct priority via figment
6. `AppPaths` returns correct XDG-compliant paths on Linux and correct macOS paths
7. PID file created on start, cleaned up on shutdown
8. Structured log output works in both pretty and JSON formats
9. Stub crates (`familiar-mcp`, `familiar-storage`) compile as part of workspace
10. `VersionInfo` populated at build time, logged at daemon startup
11. `AppStatus` updated on each heartbeat, tracks startup time and last heartbeat
