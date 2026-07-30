SHELL := /usr/bin/env bash

CARGO ?= cargo
CONFIG ?= monitor.example.toml
RPM_BUILD_SCRIPT ?= scripts/build-rpm.sh

.PHONY: help build release test check clippy fmt fmt-check doc run verify-config rpm package clean

help:
	@printf '%s\n' \
		'Targets:' \
		'  make build          Build debug binary' \
		'  make release        Build release binary' \
		'  make test           Run tests' \
		'  make check          Run cargo check' \
		'  make clippy         Run clippy with warnings denied' \
		'  make fmt            Format Rust code' \
		'  make fmt-check      Check Rust formatting' \
		'  make doc            Build docs without dependencies' \
		'  make run            Run app with CONFIG=monitor.example.toml' \
		'  make verify-config  Validate CONFIG without starting services' \
		'  make rpm            Build source and binary RPMs' \
		'  make clean          Remove cargo build artifacts'

build:
	$(CARGO) build --locked

release:
	$(CARGO) build --release --locked

test:
	$(CARGO) test --locked

check:
	$(CARGO) check --locked

clippy:
	$(CARGO) clippy --all-targets --locked -- -D warnings

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt -- --check

doc:
	$(CARGO) doc --locked --no-deps

run:
	$(CARGO) run --locked -- --config "$(CONFIG)"

verify-config:
	$(CARGO) run --locked -- --config "$(CONFIG)" --verify-config-only

rpm package:
	$(RPM_BUILD_SCRIPT)

clean:
	$(CARGO) clean
