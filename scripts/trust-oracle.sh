#!/usr/bin/env bash
#
# Check paranoid's repository verification against apt: build a throwaway
# repository signed by a throwaway key, break it in each of the ways that
# matter, and ask both `paranoid lock` and `apt-get update` whether they
# accept it.
#
# Usage: scripts/trust-oracle.sh
#
# Needs podman, gpg, python3 and xz, and a built paranoid. The image holding
# apt is built by paranoid itself. Exits non-zero if either tool disagrees
# with what we expect of it.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
paranoid=${PARANOID:-$root/target/debug/paranoid}
case ${ARCH:-$(uname -m)} in
x86_64 | amd64) arch=amd64 ;;
aarch64 | arm64) arch=arm64 ;;
*) arch=${ARCH:-$(uname -m)} ;;
esac
work=$(mktemp -d)
server=
trap '[ -n "$server" ] && kill "$server"; rm -rf "$work"' EXIT

export GNUPGHOME=$work/gnupg
mkdir -m 700 "$GNUPGHOME"
quiet_gpg() { gpg --batch --quiet --pinentry-mode loopback --passphrase '' "$@" 2>/dev/null; }

# The trusted key, a stranger's key, and a key that expired in 2021.
quiet_gpg --quick-gen-key 'paranoid trusted <trusted@example.org>' ed25519 sign never
quiet_gpg --quick-gen-key 'paranoid stranger <stranger@example.org>' ed25519 sign never
quiet_gpg --faked-system-time 20200101T000000 \
    --quick-gen-key 'paranoid expired <expired@example.org>' ed25519 sign 1y
# Binary, since apt's signed-by can't read armored keys without gpg.
quiet_gpg --export trusted@example.org expired@example.org >"$work/keyring.gpg"

# The repository: one package nobody depends on. `lock` never downloads
# it, so its checksum is made up.
packages() {
    cat <<EOF
Package: hello-paranoid
Version: 1.0
Architecture: all
Priority: optional
Filename: pool/main/h/hello-paranoid_1.0_all.deb
Size: 1000
SHA256: $(printf x | shasum -a 256 | cut -d' ' -f1)
EOF
}

# release DIST [EXTRA-FIELD...]: a Release for the indexes under dists/test.
release() {
    local dist=$1
    shift
    printf 'Origin: paranoid\nSuite: %s\nCodename: %s\n' "$dist" "$dist"
    printf 'Date: %s\n' "$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S UTC')"
    printf 'Architectures: %s\nComponents: main\n' "$arch"
    for field in "$@"; do printf '%s\n' "$field"; done
    printf 'SHA256:\n'
    for f in main/binary-$arch/Packages main/binary-$arch/Packages.xz; do
        printf ' %s %s %s\n' "$(shasum -a 256 <"$repo/dists/test/$f" | cut -d' ' -f1)" \
            "$(wc -c <"$repo/dists/test/$f" | tr -d ' ')" "$f"
    done
}

# sign KEY [GPG-OPTION...]: clearsign Release into InRelease.
sign() {
    local key=$1
    shift
    quiet_gpg --local-user "$key" "$@" --clearsign \
        -o "$repo/dists/test/InRelease" "$repo/dists/test/Release"
}

# scenario NAME APT PARANOID: a fresh, good repository in $repo; the caller
# then breaks it. APT and PARANOID are what each should do: accept/reject.
mkdir "$work/s"
scenario() {
    repo=$work/s/$1/repo
    mkdir -p "$repo/dists/test/main/binary-$arch"
    echo "$2 $3" >"$work/s/$1/expect"
    packages >"$repo/dists/test/main/binary-$arch/Packages"
    xz -k "$repo/dists/test/main/binary-$arch/Packages"
}

scenario good accept accept
release test >"$repo/dists/test/Release"
sign trusted@example.org

scenario modified-inrelease reject reject
release test >"$repo/dists/test/Release"
sign trusted@example.org
sed -i.bak 's/^Origin: paranoid$/Origin: evil/' "$repo/dists/test/InRelease"

scenario modified-packages reject reject
release test >"$repo/dists/test/Release"
sign trusted@example.org
for p in "$repo/dists/test/main/binary-$arch/Packages"; do
    sed -i.bak 's/^Version: 1.0$/Version: 6.6.6/' "$p"
    rm -f "$p.xz" "$p.bak" && xz -k "$p"
done

scenario unknown-key reject reject
release test >"$repo/dists/test/Release"
sign stranger@example.org

scenario expired-key reject reject
release test >"$repo/dists/test/Release"
sign expired@example.org --faked-system-time 20200601T000000

scenario expired-release reject reject
release test 'Valid-Until: Thu, 01 Jan 2004 00:00:00 UTC' >"$repo/dists/test/Release"
sign trusted@example.org

scenario unsigned reject reject
release test >"$repo/dists/test/Release"

scenario text-before-signature reject reject
release test >"$repo/dists/test/Release"
sign trusted@example.org
{ printf 'Origin: evil\n\n'; cat "$repo/dists/test/InRelease"; } >"$repo/dists/test/x"
mv "$repo/dists/test/x" "$repo/dists/test/InRelease"

scenario sha1-signature reject reject
release test >"$repo/dists/test/Release"
sign trusted@example.org --digest-algo SHA1

# apt only warns about a Release for another distribution; we refuse it,
# since a validly signed old snapshot is how a downgrade is served.
scenario other-distribution accept reject
release other >"$repo/dists/test/Release"
sign trusted@example.org

# The apt image. apt-key needs a shell and a few tools to run gpgv.
mkdir "$work/image"
cat >"$work/image/paranoid.toml" <<TOML
[image]
name = "trust-oracle"
architecture = "$arch"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "$root/examples/keys/debian-archive-bookworm-automatic.asc"

$(for p in apt gpgv dpkg bash coreutils dash grep sed findutils; do
    printf '[[packages]]\nname = "%s"\n\n' "$p"
done)
[entrypoint]
cmd = ["/bin/bash", "-c"]
TOML
"$paranoid" lock -q -f "$work/image"
"$paranoid" build -q -f "$work/image" -o "$work/image.tar"
podman load -q -i "$work/image.tar" >/dev/null

podman run --rm -v "$work:/w" localhost/trust-oracle:latest '
    mkdir -p /etc/apt/apt.conf.d /etc/apt/sources.list.d /var/lib/apt/lists/partial \
        /var/cache/apt/archives/partial /var/lib/dpkg /var/log/apt /tmp
    chmod 1777 /tmp
    : >/var/lib/dpkg/status
    : >/etc/apt/sources.list
    printf "%s\n" "APT::Sandbox::User \"root\";" \
        "APT::Architecture \"'"$arch"'\";" >/etc/apt/apt.conf.d/oracle
    for dir in /w/s/*/; do
        rm -rf /var/lib/apt/lists/* && mkdir -p /var/lib/apt/lists/partial
        echo "deb [signed-by=/w/keyring.gpg] file:${dir}repo test main" \
            >/etc/apt/sources.list
        apt-get update >"${dir}apt.log" 2>&1 || true
        if apt-cache show hello-paranoid 2>/dev/null | grep -q "^Version: 1.0$"; then
            echo accept >"${dir}apt"
        else
            echo reject >"${dir}apt"
        fi
    done'

port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
python3 -m http.server --bind 127.0.0.1 "$port" --directory "$work/s" >/dev/null 2>&1 &
server=$!
disown
sleep 1

failed=0
for dir in "$work"/s/*/; do
    name=$(basename "$dir")
    mkdir "$dir/lock"
    cat >"$dir/lock/paranoid.toml" <<TOML
[image]
architecture = "$arch"

[repositories.test]
url = "http://127.0.0.1:$port/$name/repo"
distribution = "test"
components = ["main"]
signed_by = "$work/keyring.gpg"

[[packages]]
name = "hello-paranoid"
TOML
    if "$paranoid" lock -q -f "$dir/lock" 2>"$dir/paranoid.log" &&
        grep -q '^version = "1.0"$' "$dir/lock/paranoid.lock"; then
        echo accept >"$dir/paranoid"
    else
        echo reject >"$dir/paranoid"
    fi

    read -r want_apt want_paranoid <"$dir/expect"
    got_apt=$(cat "$dir/apt")
    got_paranoid=$(cat "$dir/paranoid")
    if [ "$got_apt" = "$want_apt" ] && [ "$got_paranoid" = "$want_paranoid" ]; then
        status=ok
    else
        status=FAIL
        failed=1
    fi
    printf '%-5s %-22s apt %-7s paranoid %-7s %s\n' "$status" "$name" "$got_apt" \
        "$got_paranoid" "$(sed 's/.*resolution failed: //' "$dir/paranoid.log" | head -1)"
    if [ "$status" = FAIL ]; then
        grep -E '^(E|W):' "$dir/apt.log" | sed 's/^/      apt: /'
    fi
done

exit $failed
