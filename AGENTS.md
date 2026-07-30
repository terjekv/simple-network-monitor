# Repository guidance

## Scope

These instructions apply to the entire repository.

## Public repository boundary

This is a public, reusable network-monitoring server. Keep all committed
content suitable for publication:

- Do not add real host inventories, organization-specific hostnames, room
  numbers, contacts, credentials, private endpoints, or operational scripts.
- Use RFC-reserved example addresses such as `192.0.2.0/24`, `.example`
  hostnames, and clearly fake tokens in documentation and tests.
- Keep local configs, databases, environment files, keys, and certificates
  ignored. `monitor.example.toml` is the only committed monitor config.
- Preserve secure examples and defaults: loopback binding, authenticated
  non-loopback access, and SSH host-key verification.

## Development

- Keep the Rust code formatted with `cargo fmt --all -- --check`.
- Run `cargo test --locked` for behavior changes.
- Run `cargo clippy --all-targets --locked -- -D warnings` before publishing.
- Validate config changes with:
  `cargo run --locked -- --config monitor.example.toml --verify-config-only`.
- Keep `Cargo.lock` committed for reproducible application builds.

## Project structure

- `src/domain`: core host, status, usage, and filter types.
- `src/app`: query services and filter registry.
- `src/backends`: ICMP and SSH implementations.
- `src/modules`: monitoring orchestration.
- `src/storage`: SQLite persistence.
- `src/api`: HTTP routes, authentication, DTOs, and OpenAPI.
- `packaging`: systemd and RPM packaging.

Update documentation and the safe example config when public behavior or
configuration changes.
