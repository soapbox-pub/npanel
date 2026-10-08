#!/bin/sh
# Build npanel the one way a server should run it: a static musl binary with
# the dashboard compiled in, from the `release` profile. The release workflow,
# the Dockerfile and a build by hand all run this, so what is deployed is what
# a release publishes. Run from the repository root:
#
#   contrib/release/build.sh OUTDIR [ARCH...]
#
# ARCH is x86_64 or aarch64, both by default; either is built on either. Writes
# npanel-VERSION-linux-ARCH to OUTDIR, VERSION from Cargo.toml. Needs the Rust
# rust-toolchain.toml names, contrib/release/tools.sh's tools on PATH, and the
# dashboard built into dashboard/dist.
set -eu

if [ $# -lt 1 ]; then
    echo "Usage: $0 OUTDIR [ARCH...]" >&2
    exit 2
fi
outdir="$1"
shift
[ $# -gt 0 ] || set -- x86_64 aarch64

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
if [ ! -f dashboard/dist/index.html ]; then
    echo "dashboard/dist is not built. Run \`bun install && bun run build\` in dashboard/ first." >&2
    exit 1
fi

# zig's linker holds every object file open at once, and there are more than
# the default soft limit since Arti.
ulimit -n "$(ulimit -Hn)"
mkdir -p "$outdir"
for arch in "$@"; do
    case "$arch" in
        x86_64 | aarch64) ;;
        *)
            echo "Unknown architecture $arch: use x86_64 or aarch64." >&2
            exit 2
            ;;
    esac
    cargo zigbuild --release --locked --target "$arch-unknown-linux-musl" --features embedded-dashboard
    bin="$outdir/npanel-$version-linux-$arch"
    # CARGO_TARGET_DIR, where an environment sets one: some CI runners share
    # one between runs.
    cp "${CARGO_TARGET_DIR:-target}/$arch-unknown-linux-musl/release/npanel" "$bin"
    # A musl binary that still asks for a dynamic loader fails on glibc hosts
    # with a misleading "No such file or directory"; refuse to make one.
    if readelf -l "$bin" | grep -q 'program interpreter'; then
        echo "$bin is dynamically linked." >&2
        exit 1
    fi
done
ls -l "$outdir"
