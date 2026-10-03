# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
FROM docker.io/library/rust:1.99.0-alpine3.24@sha256:a96ea6d18d4062e38f16cfbadd8b4541d622f2527dd0a5eca1fb36d301da4e88 AS builder

WORKDIR /usr/src/simple-network-monitor

RUN apk add --no-cache build-base

COPY Cargo.toml Cargo.lock ./

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/usr/src/simple-network-monitor/target \
    mkdir -p src && \
    printf 'pub fn placeholder() {}\n' > src/lib.rs && \
    printf 'fn main() {}\n' > src/main.rs && \
    cargo build --locked --release && \
    rm -rf src

COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/usr/src/simple-network-monitor/target \
    find src -type f -exec touch {} + && \
    cargo build --locked --release && \
    install -Dpm0755 \
        target/release/simple-network-monitor \
        /out/simple-network-monitor && \
    strip /out/simple-network-monitor

FROM scratch AS release-artifacts

COPY --from=builder /out/simple-network-monitor /simple-network-monitor
