#!/usr/bin/env bash
#
# Check paranoid's resolver against apt: for each package, compare what
# `paranoid lock` selects with what `apt-get install --no-install-recommends`
# would install on an empty system. Both read Debian bookworm main.
#
# Usage: scripts/apt-oracle.sh [package...]
#
# Needs podman and a built paranoid. The image holding apt is built by
# paranoid itself. Exits non-zero if any package resolves differently.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
paranoid=${PARANOID:-$root/target/debug/paranoid}
arch=${ARCH:-$(uname -m)}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ $# -eq 0 ]; then
    set -- nodejs bash curl python3 openssl ca-certificates git nginx \
        postgresql-15 etcd-server perl vim coreutils apt openssh-server \
        golang-go ruby php8.2-cli default-jre-headless redis-server sudo \
        tzdata iproute2 ffmpeg imagemagick gcc make build-essential \
        libpq-dev mariadb-server
fi

# manifest NAME PACKAGE... : a bookworm manifest asking for PACKAGEs.
manifest() {
    local name=$1
    shift
    cat <<TOML
[image]
name = "$name"
architecture = "$arch"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "$root/examples/keys/debian-archive-bookworm-automatic.asc"
TOML
    for package in "$@"; do
        printf '\n[[packages]]\nname = "%s"\n' "$package"
    done
}

# apt needs dpkg's data files, which it never declares since dpkg is
# Essential, and has no /bin/sh to run apt-key, hence `trusted=yes`: we're
# only asking apt's resolver, not checking signatures.
mkdir "$work/image"
{
    manifest apt-oracle apt gpgv debian-archive-keyring dpkg bash coreutils
    printf '\n[entrypoint]\ncmd = ["/bin/bash", "-c"]\n'
} >"$work/image/paranoid.toml"
"$paranoid" lock -q -f "$work/image"
"$paranoid" build -q -f "$work/image" -o "$work/image.tar"
podman load -q -i "$work/image.tar" >/dev/null

printf '%s\n' "$@" >"$work/packages"
podman run --rm -v "$work:/w" localhost/apt-oracle:latest '
    mkdir -p /etc/apt/apt.conf.d /var/lib/apt/lists/partial \
        /var/cache/apt/archives/partial /var/lib/dpkg /var/log/apt /tmp
    : >/var/lib/dpkg/status
    echo "deb [trusted=yes] http://deb.debian.org/debian bookworm main" \
        >/etc/apt/sources.list
    printf "%s\n" "APT::Sandbox::User \"root\";" \
        "APT::Architecture \"$(dpkg --print-architecture)\";" \
        "APT::Install-Recommends \"false\";" >/etc/apt/apt.conf.d/oracle
    apt-get update -qq >/dev/null 2>&1
    while read -r package; do
        apt-get -s install "$package" >"/w/apt-$package" 2>&1
    done </w/packages'

failed=0
while read -r package; do
    mkdir "$work/$package"
    manifest "$package" "$package" >"$work/$package/paranoid.toml"
    "$paranoid" lock -q -f "$work/$package"

    sed -n 's/^Inst \([^ ]*\) (\([^ ]*\).*/\1 \2/p' "$work/apt-$package" |
        sort >"$work/$package/apt"
    awk -F'"' '/^\[\[package\]\]/ { p = 1 } /^\[\[deps\]\]/ { p = 0 }
        p && /^name = / { n = $2 } p && /^version = / { print n, $2 }' \
        "$work/$package/paranoid.lock" | sort >"$work/$package/paranoid"

    if diff -u "$work/$package/apt" "$work/$package/paranoid" >"$work/$package/diff"; then
        echo "ok    $package ($(wc -l <"$work/$package/apt" | tr -d ' ') packages)"
    else
        echo "DIFF  $package"
        sed 's/^/      /' "$work/$package/diff"
        failed=1
    fi
done <"$work/packages"

exit $failed
