# syntax=docker/dockerfile:1@sha256:ecfaec9ed6d810b56388c508f4121597bfbba70d41a6dfeee4d8cad5f295fc32
FROM docker.io/library/rust:1.98.1-alpine3.24@sha256:7cc1c22d77d9432f7fe012a70e6d3e555af54c2a6832700ed7d553f1769ae89f AS builder

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
