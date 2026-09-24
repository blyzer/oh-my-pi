#!/bin/sh
set -eu

# install-smoke-macos.sh [dist-dir]
# Installs what package-macos.sh wrote for HEAD the way a user would, without
# touching the system, and checks the result:
#   - the .sha256 manifest matches both artifacts;
#   - the tarball's install.sh installs a binary into a scratch PREFIX, and
#     that binary reports `omp/<version>` (the updater contract) and runs;
#   - the .pkg installs bin/omp at usr/local/bin/omp under the identifier
#     com.oh-my-pi.omp and carries the same version.
# The .pkg is inspected, not installed: installing it needs root and writes
# /usr/local.

if [ "$(uname -s)" != Darwin ]; then
    echo "error: install-smoke-macos.sh checks the macOS artifacts and needs macOS" >&2
    exit 2
fi

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO_ROOT"
DIST=${1:-dist}
NAME="omp-$(git rev-parse --short HEAD)-aarch64-apple-darwin"
VERSION=$(cargo metadata --no-deps --format-version 1 --locked |
    python3 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "omp-app"))')

for artifact in "$NAME.tar.gz" "$NAME.pkg" "$NAME.sha256"; do
    if [ ! -f "$DIST/$artifact" ]; then
        echo "error: $DIST/$artifact is missing; run \`just package-macos\` first" >&2
        exit 1
    fi
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/omp-install-smoke.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

echo "== checksums"
( cd "$DIST" && shasum -a 256 -c "$NAME.sha256" )

echo "== tarball install into a scratch prefix"
tar -xzf "$DIST/$NAME.tar.gz" -C "$WORK"
PREFIX="$WORK/prefix" sh "$WORK/$NAME/install.sh"
reported=$("$WORK/prefix/bin/omp" --version)
if [ "$reported" != "omp/$VERSION" ]; then
    echo "error: installed omp reports '$reported', expected 'omp/$VERSION'" >&2
    exit 1
fi
echo "omp --version: $reported"
"$WORK/prefix/bin/omp" --help >/dev/null
echo "omp --help: ok"

echo "== pkg contents"
if ! pkgutil --payload-files "$DIST/$NAME.pkg" | grep -qx './usr/local/bin/omp'; then
    echo "error: $NAME.pkg does not install usr/local/bin/omp" >&2
    exit 1
fi
pkgutil --expand "$DIST/$NAME.pkg" "$WORK/expanded"
info="$WORK/expanded/PackageInfo"
if ! grep -q 'identifier="com.oh-my-pi.omp"' "$info"; then
    echo "error: $NAME.pkg is not identified as com.oh-my-pi.omp" >&2
    exit 1
fi
if ! grep -q "version=\"$VERSION\"" "$info"; then
    echo "error: $NAME.pkg does not carry version $VERSION" >&2
    grep -o 'version="[^"]*"' "$info" >&2 || true
    exit 1
fi
echo "pkg: usr/local/bin/omp, com.oh-my-pi.omp, version $VERSION"
echo "install smoke passed for $NAME"
