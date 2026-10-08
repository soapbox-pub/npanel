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

# contrib/npanel.service and npanel.example.toml are written for an install by
# hand; the package puts the binary in /usr/bin and the data in
# /var/lib/npanel, which the npanel user's default wouldn't be. dashboard_host
# is left empty, so the service refuses to start until it is set.
sed 's|/usr/local/bin/npanel|/usr/bin/npanel|g' contrib/npanel.service > "$stage/npanel.service"
sed -e 's|^dashboard_host = .*|dashboard_host = ""|' \
    -e 's|^# data_dir = "/var/lib/npanel"$|data_dir = "/var/lib/npanel"|' \
    npanel.example.toml > "$stage/npanel.toml"
if ! grep -qx 'ExecStart=/usr/bin/npanel --config /etc/npanel/npanel.toml' "$stage/npanel.service" \
    || ! grep -qx 'dashboard_host = ""' "$stage/npanel.toml" \
    || ! grep -qx 'data_dir = "/var/lib/npanel"' "$stage/npanel.toml"; then
    echo "contrib/npanel.service or npanel.example.toml changed shape; update $0 to match." >&2
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
