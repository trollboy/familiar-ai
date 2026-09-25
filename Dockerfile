# Stage 1: Builder
FROM rust:1.93.1-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libgtk-3-dev \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p familiar-ai-daemon --bin familiar-ai-daemon

# Stage 2: Test (used by docker compose test service)
FROM rust:1.93.1-bookworm AS tester
WORKDIR /app
# familiar-ai-tray is a workspace member, so `--workspace` compiles it here
# regardless of the daemon's feature flags. It links GTK and the Ayatana
# indicator; without these the gate fails at configure time rather than
# quietly skipping the crate, which is the whole point of un-excluding it.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libgtk-3-dev libayatana-appindicator3-dev \
    libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-llvm-cov
RUN rustup component add llvm-tools-preview
# The drive's merge queue commits during integration; without an identity
# every git-exercising test fails ("unable to auto-detect email address"),
# which is what kept tests-workspace-advisory permanently red in Docker.
RUN git config --global user.email "tester@familiar-ai.invalid" \
    && git config --global user.name "familiar-ai-tester" \
    && git config --global init.defaultBranch main
COPY . .

# Stage 5: Runtime
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libgtk-3-0 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/familiar-ai-daemon /usr/local/bin/
ENTRYPOINT ["familiar-ai-daemon"]
CMD ["--foreground"]
