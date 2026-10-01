#!/usr/bin/env bash
# Validate this checkout and an explicit frontend checkout as one compatibility pair.
set -euo pipefail
backend_dir=$(cd "$(dirname "$0")/.." && pwd)
frontend_dir=$(cd "${1:-$backend_dir/../simple-network-monitor-frontend}" && pwd)
cd "$backend_dir"
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
cargo run --locked -- --config monitor.example.toml --verify-config-only
if [[ "$(cargo audit --version 2>/dev/null || true)" != *' 0.22.2' ]]; then
  cargo install --locked --version 0.22.2 cargo-audit
fi
cargo audit --deny warnings
python3 scripts/test-public-boundary.py
python3 scripts/test-public-boundary.py "$frontend_dir"
python3 scripts/test-publish-tag.py
cd "$frontend_dir"
npm ci
npm run lint
npm test
npm run build
npm audit --audit-level=low
SNM_BACKEND_BIN="$backend_dir/target/debug/simple-network-monitor" node scripts/check-pair.mjs
