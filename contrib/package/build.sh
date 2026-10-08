#!/bin/sh
# Package a release binary as a .deb and an .rpm, with nfpm
# (https://nfpm.goreleaser.com). Run from the repository root:
#
#   contrib/package/build.sh VERSION ARCH BINARY OUTDIR
#
# ARCH is x86_64 or aarch64. Writes npanel-VERSION-linux-ARCH.deb and .rpm to
# OUTDIR.
set -eu

if [ $# -ne 4 ]; then
    echo "Usage: $0 VERSION ARCH BINARY OUTDIR" >&2
    exit 2
fi
version="$1"
arch="$2"
binary="$3"
outdir="$4"

# nfpm's architectures are Go's; it names them the rpm way itself.
case "$arch" in
    x86_64) nfpm_arch=amd64 ;;
    aarch64) nfpm_arch=arm64 ;;
    *)
        echo "Unknown architecture $arch: use x86_64 or aarch64." >&2
        exit 2
        ;;
esac

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

# contrib/npanel.service and .env.example are written for an install by hand;
# the package puts the binary in /usr/bin and the data in /var/lib/npanel.
# DASHBOARD_HOST is left empty, so the service refuses to start until it is set.
sed 's|/usr/local/bin/npanel|/usr/bin/npanel|g' contrib/npanel.service > "$stage/npanel.service"
sed -e 's|^DASHBOARD_HOST=.*|DASHBOARD_HOST=""|' \
    -e 's|^DATA_DIR=.*|DATA_DIR="/var/lib/npanel"|' \
    .env.example > "$stage/env"
if ! grep -qx 'ExecStart=/usr/bin/npanel' "$stage/npanel.service" \
    || ! grep -qx 'DASHBOARD_HOST=""' "$stage/env" \
    || ! grep -qx 'DATA_DIR="/var/lib/npanel"' "$stage/env"; then
    echo "contrib/npanel.service or .env.example changed shape; update $0 to match." >&2
    exit 1
fi

cp "$binary" "$stage/npanel"
cp LICENSE contrib/package/nfpm.yaml contrib/package/*.conf contrib/package/post*.sh contrib/package/pre*.sh "$stage/"

mkdir -p "$outdir"
outdir="$(cd "$outdir" && pwd)"
export NPANEL_VERSION="$version" NPANEL_ARCH="$nfpm_arch"
for packager in deb rpm; do
    (cd "$stage" && nfpm package --packager "$packager" --target "$outdir/npanel-$version-linux-$arch.$packager")
done
