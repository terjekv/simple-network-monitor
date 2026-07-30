#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
NAME=simple-network-monitor
VERSION=$(
    sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" |
        head -n 1
)

if [[ -z "$VERSION" ]]; then
    echo "error: failed to read package version from Cargo.toml" >&2
    exit 1
fi

if ! command -v rpmbuild >/dev/null 2>&1; then
    echo "error: rpmbuild is required" >&2
    exit 1
fi

TOPDIR=${RPM_TOPDIR:-"$ROOT/target/rpmbuild"}
RPMBUILD_ARGS=(--define "_topdir $TOPDIR")
if command -v cargo >/dev/null 2>&1; then
    RPMBUILD_ARGS+=(--with local_cargo)
fi
if [[ -n "${RPMBUILD_OPTS:-}" ]]; then
    # Intended for simple flags such as: RPMBUILD_OPTS='--without local_cargo'.
    read -r -a EXTRA_RPMBUILD_ARGS <<< "$RPMBUILD_OPTS"
    RPMBUILD_ARGS+=("${EXTRA_RPMBUILD_ARGS[@]}")
fi

mkdir -p "$TOPDIR"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

git -C "$ROOT" archive \
    --format=tar.gz \
    --prefix="$NAME-$VERSION/" \
    --output="$TOPDIR/SOURCES/$NAME-$VERSION.tar.gz" \
    HEAD

rpmbuild \
    "${RPMBUILD_ARGS[@]}" \
    -ba "$ROOT/packaging/rpm/$NAME.spec"

echo "RPMs written under $TOPDIR/RPMS"
echo "SRPMs written under $TOPDIR/SRPMS"
