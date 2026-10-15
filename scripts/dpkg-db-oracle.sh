#!/usr/bin/env bash
#
# Check the dpkg database paranoid writes against the one dpkg writes:
# install the same packages with apt on a real Debian system and with
# `paranoid build`, then compare, package by package, the `status` stanza and
# the `info/` files (`.list`, `.md5sums`, `.conffiles`).
#
# Usage: scripts/dpkg-db-oracle.sh [package...]
#
# Needs podman and a built paranoid. The real system is the official
# debian:bookworm image, upgraded first so its versions match the mirror's.
# Only packages at the same version on both sides are compared. Exits
# non-zero on any difference.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
paranoid=${PARANOID:-$root/target/debug/paranoid}
arch=${ARCH:-$(uname -m)}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ $# -eq 0 ]; then
    # Conffiles (netbase, openssl, login, adduser), Multi-Arch: same
    # (libc6, libssl3), Essential (bash, coreutils), long descriptions and
    # extra fields (tzdata, curl).
    set -- bash coreutils netbase openssl login adduser tzdata curl \
        ca-certificates libc6 libssl3 python3-minimal
fi

mkdir "$work/image"
{
    cat <<TOML
[image]
name = "dpkg-db-oracle"
architecture = "$arch"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "$root/examples/keys/debian-archive-bookworm-automatic.asc"

[repositories.updates]
url = "https://deb.debian.org/debian"
distribution = "bookworm-updates"
components = ["main"]
signed_by = "$root/examples/keys/debian-archive-bookworm-automatic.asc"
TOML
    for package in "$@"; do
        printf '\n[[packages]]\nname = "%s"\n' "$package"
    done
} >"$work/image/paranoid.toml"
"$paranoid" lock -q -f "$work/image"
"$paranoid" build -q -f "$work/image" -o "$work/image.tar"

# paranoid's database, out of the image's single layer.
mkdir -p "$work/paranoid"
layer=$(tar -xOf "$work/image.tar" index.json |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["manifests"][0]["digest"])')
manifest=$(tar -xOf "$work/image.tar" "blobs/sha256/${layer#sha256:}" |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["layers"][0]["digest"])')
tar -xOf "$work/image.tar" "blobs/sha256/${manifest#sha256:}" |
    tar -xzf - -C "$work/paranoid" var/lib/dpkg

# dpkg's, from a real system. No security suite, matching the manifest.
# Every package in the lock is (re)installed, so dpkg writes each stanza
# itself instead of keeping what the base image was built with.
mkdir -p "$work/dpkg"
awk -F'"' '/^\[\[/ { p = /^\[\[package\]\]/ } p && /^name = / { print $2 }' \
    "$work/image/paranoid.lock" >"$work/packages"
podman run --rm -v "$work:/w" docker.io/library/debian:bookworm bash -c '
    set -e
    rm -f /etc/apt/sources.list.d/*
    printf "deb http://deb.debian.org/debian %s main\n" bookworm bookworm-updates \
        >/etc/apt/sources.list
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq >/dev/null
    apt-get -y -qq upgrade >/dev/null
    xargs apt-get -y -qq install --reinstall --no-install-recommends \
        </w/packages >/dev/null
    cp -a /var/lib/dpkg/status /var/lib/dpkg/info /w/dpkg/'

python3 - "$work" <<'PY'
import os, sys

work = sys.argv[1]

def stanzas(path):
    out = {}
    for stanza in open(path).read().split("\n\n"):
        if stanza.strip():
            name = stanza.split("\n", 1)[0].removeprefix("Package: ")
            out[name] = stanza.strip("\n")
    return out

def version(stanza):
    return next(l for l in stanza.splitlines() if l.startswith("Version: "))

ours = stanzas(f"{work}/paranoid/var/lib/dpkg/status")
theirs = stanzas(f"{work}/dpkg/status")
ok = diff = 0
skipped = []
for name in sorted(ours):
    if name not in theirs or version(ours[name]) != version(theirs[name]):
        skipped.append(name)
        continue
    problems = []
    if ours[name] != theirs[name]:
        problems.append("status stanza:\n  paranoid: " + ours[name].replace("\n", "\n            ")
                        + "\n  dpkg:     " + theirs[name].replace("\n", "\n            "))
    info = f"{work}/paranoid/var/lib/dpkg/info"
    for f in sorted(os.listdir(info)):
        base, ext = f.rsplit(".", 1) if "." in f else (f, "")
        if base.split(":")[0] != name or ext not in ("list", "md5sums", "conffiles"):
            continue
        mine = open(f"{info}/{f}", "rb").read()
        try:
            real = open(f"{work}/dpkg/info/{f}", "rb").read()
        except FileNotFoundError:
            problems.append(f"{f}: dpkg has no such file")
            continue
        if ext == "list":
            # dpkg's list order isn't meaningful; compare the sets.
            mine, real = sorted(mine.splitlines()), sorted(real.splitlines())
        if mine != real:
            problems.append(f"{f} differs")
    if problems:
        diff += 1
        print(f"DIFF  {name}")
        for p in problems:
            print("      " + p.replace("\n", "\n      "))
    else:
        ok += 1
print(f"{ok} identical, {diff} different")
if skipped:
    print(f"not compared, missing or at another version on the dpkg side: {' '.join(skipped)}")
sys.exit(1 if diff else 0)
PY
