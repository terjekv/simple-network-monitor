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

## Verification

- Keep the Rust code formatted with `cargo fmt --all -- --check`.
- Run `cargo test --locked` for behavior changes.
- Run `cargo clippy --all-targets --locked -- -D warnings` before publishing.
- Validate config changes with:
  `cargo run --locked -- --config monitor.example.toml --verify-config-only`.
- Check documentation with:
  `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps`.
- Prefer a small targeted test while iterating, then run the full suite before
  considering a behavior change complete.
- Keep `Cargo.lock` committed for reproducible application builds.
- Run `actionlint` after changing GitHub Actions workflows.
- Use the latest supported major-version refs for GitHub Actions (for example,
  `actions/checkout@v7`) and keep container images pinned to immutable digests.
- Preserve the static-link checks when changing Linux artifact builds.

## Security and dependencies

- Treat authentication, API binding, raw-socket capabilities, SSH execution,
  host-key verification, configuration reloads, and secret redaction as
  security-sensitive behavior. Add focused regression tests when changing them.
- For dependency changes, install the pinned audit tool with
  `cargo install --locked --version 0.22.2 cargo-audit`, then run
  `cargo audit --deny warnings`.
- Investigate every advisory. Do not add an advisory ignore without documenting
  why the affected code is unreachable or otherwise non-exploitable here.
- Keep CI permissions minimal. Do not weaken CodeQL, dependency review,
  RustSec, static-link, or release-gating checks merely to make CI green.
- Keep third-party GitHub Actions on their latest supported major-version refs.
  Keep Dependabot configured for both Cargo and GitHub Actions updates so major
  upgrades remain visible.

## Static and RPM builds

- Treat `Cargo.toml`, `Cargo.lock`, `src/`, `packaging/static/Dockerfile`, the
  RPM spec, and `scripts/build-rpm.sh` as inputs to published Linux artifacts.
- A normal host `cargo build` is not a substitute for the musl container build.
  When static packaging changes, build the `release-artifacts` target and verify
  the executable has neither a `DT_NEEDED` entry nor a `PT_INTERP` header.
- RPM release builds must package the already verified static executable
  byte-for-byte. Preserve the CI payload comparison and the Rocky 8 build
  environment unless deliberately changing the compatibility baseline.
- Keep rolling `main-latest` artifacts separate from immutable `v*` releases.
  Tagged releases must match `Cargo.toml` and point at a successful `main` CI
  commit.

## Rust and architecture conventions

- Keep domain types and invariants in `src/domain`, orchestration in `src/app`
  and `src/modules`, external process/network behavior in `src/backends`,
  persistence in `src/storage`, and HTTP concerns in `src/api`.
- Prefer small explicit APIs, validated newtypes, and private representation
  details when values carry domain or security invariants.
- Keep validation close to the data it protects. Reject invalid state at
  constructors, setters, or configuration/API boundaries.
- Do not add unused code or `#[allow(dead_code)]` simply to make a build or test
  pass.
- Use conventional Rust module discovery (`foo.rs` or `foo/mod.rs`); do not add
  `#[path = "..."]` overrides.

## Tests and change discipline

- Keep each test focused on one behavior. Use `rstest` cases when the same
  behavior varies by input.
- Prefer deterministic in-memory storage and narrow backend fakes over tests
  that depend on local hosts, credentials, network access, or machine state.
- Add regression coverage for bug fixes and externally visible behavior
  changes.
- Keep edits scoped to the task and prefer clear, idiomatic code over clever
  abstractions.

## Project structure

- `src/domain`: core host, status, usage, and filter types.
- `src/app`: query services and filter registry.
- `src/backends`: ICMP and SSH implementations.
- `src/modules`: monitoring orchestration.
- `src/storage`: SQLite persistence.
- `src/api`: HTTP routes, authentication, DTOs, and OpenAPI.
- `packaging`: systemd and RPM packaging.
- `packaging/static`: reproducible musl release builds.
- `.github/workflows`: validation, artifact, and release automation.

Update documentation and the safe example config when public behavior or
configuration changes.
