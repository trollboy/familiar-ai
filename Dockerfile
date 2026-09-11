# Stage 1: Builder
FROM rust:1.88-bookworm AS builder
WORKDIR /app
# Kept identical to the README install command and scripts/gate-build.sh —
# see scripts/check-feature-parity.sh.
COPY . .
# PRD-092:PARITY
RUN cargo build --release --bin familiar-ai-daemon

# Stage 2: Test (used by docker compose test service)
FROM rust:1.88-bookworm AS tester
WORKDIR /app
RUN cargo install cargo-llvm-cov
RUN rustup component add llvm-tools-preview
# PRD-092 f3: the daemon's `tray` feature needs these at configure/link time
# (see crates/familiar-ai-tray/build.rs and src/sysdeps.rs). Without them
# here, tests-green-crates/tests-workspace-advisory only ever exercise the
# named-missing-dependency branch and never compile the
# `#[cfg(feature = "tray")]` code in familiar-ai-daemon/src/main.rs.
RUN apt-get update && apt-get install -y --no-install-recommends libgtk-3-dev libxdo-dev \
    && rm -rf /var/lib/apt/lists/*
# The drive's merge queue commits during integration; without an identity
# every git-exercising test fails ("unable to auto-detect email address"),
# which is what kept tests-workspace-advisory permanently red in Docker.
RUN git config --global user.email "tester@familiar-ai.invalid" \
    && git config --global user.name "familiar-ai-tester" \
    && git config --global init.defaultBranch main
COPY . .

# Stage 5: Runtime
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/familiar-ai-daemon /usr/local/bin/
ENTRYPOINT ["familiar-ai-daemon"]
CMD ["--foreground"]
