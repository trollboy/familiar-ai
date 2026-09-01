# Stage 1: Builder
FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --no-default-features --bin familiar-ai-daemon

# Stage 2: Test (used by docker compose test service)
FROM rust:1.88-bookworm AS tester
WORKDIR /app
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
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/familiar-ai-daemon /usr/local/bin/
ENTRYPOINT ["familiar-ai-daemon"]
CMD ["--foreground"]
