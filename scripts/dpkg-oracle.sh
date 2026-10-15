#!/usr/bin/env bash
#
# Regenerate src/repository/debian/testdata/dpkg-compare-versions.txt: ask a
# real dpkg how it orders a set of version pairs, so the tests can check our
# port of verrevcmp against it without dpkg installed.
#
# Needs podman and a built paranoid. The image holding dpkg is built by
# paranoid itself.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
paranoid=${PARANOID:-$root/target/debug/paranoid}
out=$root/src/repository/debian/testdata/dpkg-compare-versions.txt
arch=${ARCH:-$(uname -m)}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cat >"$work/paranoid.toml" <<TOML
[image]
name = "dpkg-oracle"
architecture = "$arch"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]

[[packages]]
name = "dpkg"

[[packages]]
name = "bash"

[entrypoint]
cmd = ["/bin/bash", "-c"]
TOML

"$paranoid" lock -q -f "$work"
"$paranoid" build -q -f "$work" -o "$work/image.tar"
podman load -q -i "$work/image.tar" >/dev/null

# The pairs listed in version.rs's tests, then random ones that mix digits,
# letters, `~`, `+`, `.`, hyphens and epochs.
python3 -I - "$root/src/repository/debian/version.rs" >"$work/pairs.txt" <<'PY'
import random, re, sys

src = open(sys.argv[1]).read()

def table(name):
    body = src[src.index(f"const {name}"):]
    return re.findall(r'\("([^"]*)", "([^"]*)"\)', body[: body.index("];")])

alpha = "0123456789" * 3 + "abz.+~-"

def part(n):
    return "".join(random.choice(alpha) for _ in range(random.randint(0, n)))

def version():
    s = random.choice("0123456789") + part(6)
    if random.random() < 0.5:
        s += "-" + random.choice("0123456789ab~") + part(4)
    if random.random() < 0.2:
        s = f"{random.randint(0, 2)}:{s}"
    s = s.replace("--", "-")
    return s + "1" if s.endswith("-") else s

random.seed(7)
pairs = table("ORDERED") + table("EQUAL")
pairs += [(version(), version()) for _ in range(3000)]
for a, b in pairs:
    print(a, b)
PY

# Exit status 2 means dpkg rejected a version; keep it visible as `err`.
podman run --rm -i localhost/dpkg-oracle:latest '
    while read -r a b; do
        dpkg --compare-versions "$a" lt "$b" 2>/dev/null && r=lt || r=$?
        if [ "$r" = 1 ]; then
            dpkg --compare-versions "$a" eq "$b" 2>/dev/null && r=eq || r=gt
        elif [ "$r" = 2 ]; then
            r=err
        fi
        echo "$a $b $r"
    done' <"$work/pairs.txt" >"$work/results.txt"

version=$(podman run --rm localhost/dpkg-oracle:latest 'dpkg --version' |
    sed -n '1s/.*version \([^ ]*\) (\(.*\))\./\1 (Debian bookworm, \2)/p')
{
    echo '# Version pairs and how `dpkg --compare-versions` orders them (lt, eq, gt).'
    echo "# Produced by dpkg $version with scripts/dpkg-oracle.sh:"
    echo '# the pairs from the tests in version.rs, then 3000 random ones.'
    cat "$work/results.txt"
} >"$out"

echo "wrote $(grep -vc '^#' "$out") pairs to $out"
