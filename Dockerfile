# Stage 1: Builder
FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --no-default-features --bin familiar-daemon

# Stage 2: Test (used by docker compose test service)
FROM rust:1.88-bookworm AS tester
WORKDIR /app
RUN cargo install cargo-llvm-cov
RUN rustup component add llvm-tools-preview
COPY . .

# Stage 5: Runtime
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/familiar-daemon /usr/local/bin/
ENTRYPOINT ["familiar-daemon"]
CMD ["--foreground"]
