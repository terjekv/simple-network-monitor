#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
NAME=simple-network-monitor
VERSION=$(
    sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" |
        head -n 1
)
PACKAGE_VERSION=${RPM_PACKAGE_VERSION:-"$VERSION"}
PACKAGE_RELEASE=${RPM_PACKAGE_RELEASE:-}
PREBUILT_BINARY=${RPM_PREBUILT_BINARY:-}
SOURCE_ARCHIVE=${RPM_SOURCE_ARCHIVE:-}

if [[ -z "$VERSION" ]]; then
    echo "error: failed to read package version from Cargo.toml" >&2
    exit 1
fi
if [[ -z "$PACKAGE_VERSION" ]]; then
    echo "error: RPM_PACKAGE_VERSION must not be empty" >&2
    exit 1
fi

if ! command -v rpmbuild >/dev/null 2>&1; then
    echo "error: rpmbuild is required" >&2
    exit 1
fi

TOPDIR=${RPM_TOPDIR:-"$ROOT/target/rpmbuild"}
BUILD_HOST=${RPM_BUILD_HOST:-localhost}
RPMBUILD_ARGS=(
    --define "_topdir $TOPDIR"
    --define "_buildhost $BUILD_HOST"
)
RPMBUILD_MODE=(-ba)
if [[ -n "$PREBUILT_BINARY" ]]; then
    if [[ ! -f "$PREBUILT_BINARY" || ! -x "$PREBUILT_BINARY" ]]; then
        echo "error: RPM_PREBUILT_BINARY must be an executable file: $PREBUILT_BINARY" >&2
        exit 1
    fi
    RPMBUILD_ARGS+=(--with prebuilt)
    RPMBUILD_MODE=(-bb)
elif command -v cargo >/dev/null 2>&1; then
    RPMBUILD_ARGS+=(--with local_cargo)
fi
RPMBUILD_ARGS+=(--define "package_version $PACKAGE_VERSION")
if [[ -n "$PACKAGE_RELEASE" ]]; then
    RPMBUILD_ARGS+=(--define "package_release $PACKAGE_RELEASE")
fi
if [[ -n "${RPMBUILD_OPTS:-}" ]]; then
    # Intended for simple flags such as: RPMBUILD_OPTS='--without local_cargo'.
    read -r -a EXTRA_RPMBUILD_ARGS <<< "$RPMBUILD_OPTS"
    RPMBUILD_ARGS+=("${EXTRA_RPMBUILD_ARGS[@]}")
fi

mkdir -p "$TOPDIR"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

source_destination="$TOPDIR/SOURCES/$NAME-$PACKAGE_VERSION.tar.gz"
if [[ -n "$SOURCE_ARCHIVE" ]]; then
    if [[ ! -f "$SOURCE_ARCHIVE" ]]; then
        echo "error: RPM_SOURCE_ARCHIVE must be a file: $SOURCE_ARCHIVE" >&2
        exit 1
    fi
    install -m 0644 "$SOURCE_ARCHIVE" "$source_destination"
else
    git -C "$ROOT" archive \
        --format=tar.gz \
        --prefix="$NAME-$PACKAGE_VERSION/" \
        --output="$source_destination" \
        HEAD
fi

if [[ -n "$PREBUILT_BINARY" ]]; then
    install -m 0755 "$PREBUILT_BINARY" "$TOPDIR/SOURCES/$NAME"
fi

rpmbuild \
    "${RPMBUILD_ARGS[@]}" \
    "${RPMBUILD_MODE[@]}" \
    "$ROOT/packaging/rpm/$NAME.spec"

echo "RPMs written under $TOPDIR/RPMS"
echo "SRPMs written under $TOPDIR/SRPMS"
