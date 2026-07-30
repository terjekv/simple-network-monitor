# syntax=docker/dockerfile:1
FROM docker.io/library/rust:1.97.0-alpine3.24@sha256:ec9c91e77119ce498cd1e87d96d77e0f75b2cee21655a29bc2bf75a51a2b20a4 AS builder

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
