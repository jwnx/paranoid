```
..................................................................................:::
....................................::::----:.....................................:::
................................:-----=-===----:.................................::::
..............................--======+=-+##+-===-..............................::::-
.............................==+**#%#+=:..-+#%#**+=-............................::::-
............................+++%%%#=:.......-+%%###+=:........................::::::-
...........................=+##%%*-..........:+#%%%%*+-.......................::::---
..........................=#*#%@#=............-+#%@@%#*-.................:..:.:::----
..........................###%%%*-..........:-=*%%@@@%#+:.................:..:::-:---
.........................+%%%%@@%*=-:......-==+#%@@@@%%%=..................::::-:----
.........:=..............#%%%@@@@@#+=-::::-+%%%@@@@@@@%%*:...................::::::::
....--#@@@@@@@#=........-%@%%@@@@@++=*+:..=*==#*@@@@@@%@#-.................:.:::::::-
..==@@%%##*+++*###=.....=%%%%@@%%@%##*=:..:---===#@@@@@@%+.................:::::::::-
+=*%#==---:........##-..=%@@%@@#*==-::..........:+@@@@@@@#-..............:.:::::::---
+%#-.................=*.+@@@@@@%+-:.............:=%@@@@@@%+................::::::----
=#+.......:.....--::-==##@@@@@@@*=:..--+--==-:..:-*@@@@@@@#:...........-+#+--++-===#%
#-..:=:...-:...-#%%#+==-+@@@@@@@#=-::.:--:::...:::=#@@@@@@%*=:.---:..=%#-.......*#%@@
#:..-#....*+...=@@@@@@%#%@@@@@@@*=-:::...:..:::::--+@@@@@@@%++=-..........:.......-#@
=..=%+...-#*:-=#@@@@@%++@@@@@@@@#=---==========----+@@@@@@@%#=%%+++++=---==-........-
.:*@#.:-#@#%%%%#@@@@%--+@@@@%@@@%+=--:--------:::-=#@@@@@@@@%*%=-=#-=%@@@@%%%...-:::.
@@@@@@@@@%#%%@@@@@@@@@##@@@@@@@@@@+=--:.......:--+%@@@@@@@@@%#-=%@=-+%@@@@@@@@%#=-...
@@@@@@@@@@@%%@@@@@@@@%%@@@@@@@@@@@@@#=--:-----=+%@@@@@@@@@@@@@@@%*%@@@@@@@@@@@%#=:...
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@%@%#++++++#@@@@@@@@@@@@@@@@@@@@@@@@@@@@%@@@@@@+:.
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@%%%+===*%@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@+-
%@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@%%#+++%@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@%-
-----=+%@@@@@@@@@@@@@@@@@@@@@@@@@@%@@@@@@@@#@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
----------=#@@@@@@@@@@@@@@@@@@@@@@%#@@@@@@@+%@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@%*+++
------------=%@@@@@@@@@@@@@@@@@@@@@#%@@@@@@+@@@@@@%@@@@@@@@@@@@@@@@@@@@@+-===========
=====-=-------=%@@@@@@@@@@@@@@@@@@@@#%@@@@@%@@@@@%@@@@@@@@@@@@@@@@@@@@+--============
============---=%@@@@@@@@@@@@@@@@@@@@#%@@@%%@@@@%%@@@@@@@@@@@@@@@@@@@+===============
=============----@@@@@@@@@@@@@@@@@@@@@#@@@#%@@@#@@@@@@@@@@@@@@@@@@@@=================
================-=@@@@@@@@@@@@@@@@@@@@@+@@#@@@%@@@@@@@@@@@@@@@@@@@%==-===============
```

```
PARANOID(1)                     User Commands                     PARANOID(1)
```

# paranoid

Build reproducible OCI images from Debian packages.

paranoid builds an image without running a container, a shell or a package
manager. You list the packages you need in `paranoid.toml`, along with the
repositories to get them from and the keys that sign those repositories.
`paranoid lock` checks each repository's signature, resolves dependencies,
and writes every package's exact version and SHA-256 to `paranoid.lock`.
`paranoid build` downloads those packages, checks them against the lock,
unpacks them and writes the image. Building the same lock twice gives the
same image digest.

The images it makes are in the style of
[distroless](https://github.com/GoogleContainerTools/distroless): your
application and its runtime libraries, and nothing else unless you add it.
If you want a shell in the image, add one, like `bash`, to the package list.

```
  paranoid.toml ──lock──▶ paranoid.lock ──build──▶ image.tar
```

## SYNOPSIS

```
paranoid lock    [-f MANIFEST] [-o LOCK] [--force] [--allow-unmet] [--json]
                 [--snapshot TIME] [--upgrade] [--cache-dir DIR]
paranoid build   [-f MANIFEST] [--lock LOCK] [-o OUT] [--image ID]...
                 [--cache-dir DIR] [--jobs N] [--force-download]
                 [--allow-stale] [--allow-unmet]
paranoid cache push    [URL|DIR] [-f MANIFEST] [--lock LOCK] [--cache-dir DIR]
                       [--jobs N]
paranoid diff    OLD NEW [--json]
paranoid inspect lock  [FILE] [--tree] [--image ID]
paranoid inspect image [PATH]
```

`-q` silences everything but errors. `--help` on any command has the rest.

## QUICKSTART

```sh
$ cargo install --path .
$ cd examples/node
$ paranoid lock
$ paranoid build
$ podman load -i image.tar
$ podman run --rm localhost/node:latest --version
v18.20.4
```

`examples/node` resolves to 18 packages and a ~37 MB arm64 image. Change
`architecture` in its manifest if you're on amd64.

## MANIFEST

`paranoid.toml` (or `.yaml`, `.yml`, `.json`; exactly one per directory).
A misspelled or unknown field makes paranoid reject the manifest.

```toml
paranoid     = "~0.5"            # optional, the paranoid versions to use

[image]
name         = "node"            # optional, also the title label
tag          = "latest"          # optional, defaults to latest
architecture = "arm64"           # required; amd64, arm64, armhf, ...
description  = "..."             # optional, OCI label
authors      = "..."             # optional, OCI label
source       = "..."             # optional, OCI label
unconfigured = false             # optional, see BUILD
output       = "node.tar"        # optional, see BUILD

[repositories.debian]            # on a tie, the first repository wins
url          = "https://deb.debian.org/debian"
distribution = "bookworm"
components   = ["main"]
signed_by    = "keys/debian-archive-bookworm-automatic.asc"
priority     = 500               # optional, apt's pin priority

[[packages]]
name       = "nodejs"
version    = ">= 18"             # optional: "1.2-3" or "(>=|<=|>>|<<|=) 1.2"
repository = "debian"            # optional pin to one repository

[entrypoint]
cmd     = ["node"]               # becomes the image ENTRYPOINT
workdir = "/app"
env     = { NODE_ENV = "production" }
```

Image digests depend on the paranoid that builds them as well as on the
lock: a new release may compress differently. `paranoid` takes a Cargo
version requirement, and every command refuses a manifest that the running
binary doesn't meet, rather than quietly producing another digest.

`signed_by` is required. It names a file of OpenPGP public keys, armored or
binary, relative to the manifest; apt calls the same thing `Signed-By`.
Only those keys are trusted for that repository, and paranoid ships none of
its own. For Debian, the files in `/etc/apt/trusted.gpg.d/` of the
`debian-archive-keyring` package work as they are, and
<https://ftp-master.debian.org/keys.html> lists the same keys. Check the
fingerprint before you commit a key:

```
$ gpg --show-keys examples/keys/debian-archive-bookworm-automatic.asc
pub   rsa4096 2023-01-21 [SC] [expires: 2031-01-19]
      B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8
      ...
uid                      Debian Archive Automatic Signing Key (12/bookworm) <ftpmaster@debian.org>
```

`[holds]` puts a version or a repository on a package wherever it ends
up, whether the manifest asks for it or another package pulls it in:

```toml
[holds]
pgbackrest = "= 2.58.0-1.pgdg13+1"
libpq5     = { repository = "pgdg" }
```

A hold that matches no package in the result is an error, since it's
usually a misspelled name.

Binary packages built from the same source package must come from the same
version of it, so a `postgresql-17` and a `postgresql-server-dev-17` from
different releases can't end up side by side. apt allows that; paranoid
reports it and asks for a hold. The rule compares source versions, since a
binNMU legitimately gives some binaries of a source a `+bN` version.

When several repositories carry a package, the one with the highest
`priority` wins, then the highest version, the way apt picks a candidate.
A negative priority means the repository is never used. Without
`priority` you get apt's default: 500, or 100 for a repository whose
Release says `NotAutomatic` and `ButAutomaticUpgrades` (backports), or 1 for
one that only says `NotAutomatic` (experimental). A low priority is how you
keep an archive of old versions around for the few packages you hold back,
without its newer or pre-release versions creeping in.

Some repositories drop old versions from their pool, which breaks every
lock that still names one. PGDG moves them to apt-archive. `mirrors` lists
more base URLs serving the same pool, which `build` tries in order when the
repository doesn't have a package or is down:

```toml
[repositories.pgdg]
url          = "https://apt.postgresql.org/pub/repos/apt"
distribution = "trixie-pgdg"
components   = ["main"]
signed_by    = "keys/pgdg.asc"
mirrors      = ["https://apt-archive.postgresql.org/pub/repos/apt"]
```

Whatever a mirror serves is checked against the lock like anything else,
and mirrors play no part in resolution, so adding one doesn't make the
lock stale.

`build` tries each source up to three times when the failure might go
away: a 5xx other than 501 or 505, a 429 or 408, a timeout, a refused or
dropped connection. It waits half a second, then a second, and then moves
on to the next source. Any other answer that isn't a 2xx, such as a 404, a
403 or a redirect it couldn't follow, moves on at once. A source still
failing after its retries goes last for the rest of the build, with a
single attempt, so an outage costs the first few packages their retries
rather than every package, and a source that comes back is still found.

A source that serves bytes that don't match the lock, or more bytes than
the lock says, stops the build there without trying the rest, since it's
broken or tampered with. When no source has a package, the error lists
what each one answered.

To lock against a fixed point in a repository's history, point `url` at a
snapshot service and add `snapshot`. The repository becomes `url/snapshot`,
so moving to a newer snapshot is a one-line change:

```toml
[repositories.debian]
url          = "https://snapshot.debian.org/archive/debian"
snapshot     = "20261005T000000Z"
distribution = "trixie"
components   = ["main"]
signed_by    = "keys/debian-archive-trixie-automatic.asc"
```

An old snapshot's Release is usually past its `Valid-Until`. With
`snapshot` set, paranoid checks that the Release was valid at the
snapshot's time instead of now, and that it isn't dated after it.

`paranoid lock --snapshot now` moves every repository that has a
`snapshot` to the current time and re-locks, and `--snapshot
20261020T000000Z` moves them to that time instead. It changes only the
`snapshot` values and leaves the rest of the manifest as written,
comments included. It writes the manifest only once the new lock
resolves, so a failed resolution leaves both files as they were. It
needs a TOML manifest.

`cmd` is the image's *entrypoint*: arguments to `podman run` are appended
to it, they don't replace it.

Architectures take Debian names or the usual aliases (`x86_64`, `aarch64`,
`ppc64le`, ...) and map to the right OCI platform, variants included
(`armel` → `arm/v5`, `armhf` → `arm/v7`). `all` and `any` are not real
targets and are rejected.

## IMAGE SETS

Images built from the same repositories usually have to agree with each
other. A database server and the image that compiles its extensions need
the same server headers, and a backup tool needs the same version on every
host. One manifest can describe the whole family, and one lock then covers
it:

```toml
[repositories.debian]                    # shared, as before
...

[defaults]                               # every image starts from these
architecture = "amd64"                   # one architecture for the set
unconfigured = true

[holds]                                  # apply to every image
pgbackrest = "= 2.58.0-1.pgdg13+1"

[groups]                                 # named package lists
essential = ["base-files", "bash", "coreutils", ...]
runtime   = ["locales", "pgbackrest", ...]

[images.pg17]
name     = "mpg-pg17-base"
groups   = ["essential", "runtime"]
packages = ["postgresql-17"]

[images.pg17-builder]
name     = "mpg-pg17-builder-base"
groups   = ["essential"]
packages = ["build-essential", "postgresql-server-dev-17"]
```

An image may set `name`, `tag`, `description`, `authors`, `source`,
`unconfigured`, `entrypoint` and `output`, which override `[defaults]`.
`examples/postgres` is a small working set.

`paranoid lock` fetches and verifies every repository once and resolves
each image against that same state, in manifest order. A package has one
version across the set: whichever image selects it first fixes its version
for the rest. If a later image can't live with that version, paranoid
resolves it on its own, holds the whole set to the versions it picks and
starts over; when the earlier images can't live with those either, it's an
error naming both images. Each image still gets exactly its own
dependencies, so a builder's compilers never reach a runtime image. The
whole set locks together and goes stale together; there's no re-locking one
image on its own, since that is how two images end up resolved against
different states of a moving repository.

`paranoid build` builds every image, or the ones named with `--image`.
`{image}` in `-o` or `output` is replaced by the image's id. `-o` needs it
when building more than one image, and two images can't share an `output`;
with neither, images go to `{image}.tar`.

When the lock changes, `paranoid lock` lists what changed in which images,
including a package that only moved into or out of one.

## FILES

Not everything comes from apt: a static binary from a vendor's releases,
or one tool out of a release tarball. A manifest can fetch those straight
from a URL, pinned by their SHA-256:

```toml
[files.tini]
url    = "https://github.com/krallin/tini/releases/download/v0.19.0/tini-static-arm64"
sha256 = "eae1d3aa50c48fb23b8cbdf4e369d0910dfc538566bfd09df89a774aa84a48b9"
path   = "/usr/bin/tini"
mode   = "0755"

[files.etcdctl]
url     = "https://github.com/etcd-io/etcd/releases/download/v3.6.16/etcd-v3.6.16-linux-arm64.tar.gz"
sha256  = "ad756ff27ba21e3488cfb84d00f3f4ad8d9b60a93e20f5be5b230a4aab1dc053"
extract = "etcd-v3.6.16-linux-arm64/etcdctl"
path    = "/usr/local/bin/etcdctl"
```

- `path` is where the file goes in the image.
- Without `extract`, the download itself is the file. It gets `mode`,
  `0644` by default.
- `extract = "dir/member"` takes one member of a tar archive (plain,
  gzip, xz or zstd). The member keeps its mode from the archive unless
  `mode` is set.
- `extract = "dir/"`, ending in `/`, takes everything under that
  directory and puts it under `path`, keeping the archive's modes,
  symlinks and hard links.

Everything a file brings is owned by root. A single-image manifest gets
every file. In a set, each image names the ones it wants with
`files = ["tini"]`, and an image may have files and no packages.

`paranoid lock` downloads each file, checks it against `sha256` and
records it in the lock with its size. With only `sha256`, the hash in the
manifest is all that vouches for a file, which the lock records as
`trust = "pinned"`. Review a new hash the way you would a new dependency.

When the publisher signs its releases, name the keys with `signed_by`, as
for a repository, and say where the signature is:

```toml
[files.go]                                   # a signature over the file
url       = "https://go.dev/dl/go1.27.3.linux-arm64.tar.gz"
signature = "https://go.dev/dl/go1.27.3.linux-arm64.tar.gz.asc"
signed_by = "keys/google-linux.asc"
extract   = "go/"
path      = "/usr/local/go"

[files.node]                                 # a clearsigned checksums file
url       = "https://nodejs.org/dist/v26.13.0/node-v26.13.0-linux-arm64.tar.xz"
checksums = "https://nodejs.org/dist/v26.13.0/SHASUMS256.txt.asc"
signed_by = "keys/node-release.asc"
extract   = "node-v26.13.0-linux-arm64/bin/node"
path      = "/usr/local/bin/node"
```

With both `checksums` and `signature`, the signature is over the
checksums file, as with `SHA256SUMS` and `SHA256SUMS.sig`. A checksums
file lists the file by the last part of its URL. `lock` checks the
signature the way it checks a repository's, with one difference: a key
only has to have been valid when it signed, since a release outlives the
key that signed it. A signature dated before its key was made, or in the
future, doesn't count, and neither does a revoked key. A signature over
the file itself has to be a binary one: a text signature ignores line
endings, so it would vouch for bytes nobody signed. The lock records the
hash with `trust = "signed"` and the signers' fingerprints, and stays
current only while `signed_by` still has one of them that isn't revoked;
add a revocation to `signed_by` and the next `lock` checks the file again.
`sha256` is optional for a signed file; when it's there, the signature
has to agree with it. `lock --force` checks every file again.

A signature says who made a file, not which release it is: anything the
key ever signed checks out, and so does an older checksums file that lists
the same name. Prefer URLs and checksums names with the version in them,
like the ones above, over `latest` links, or pin `sha256` as well.

Publishers retire keys from the files they publish. Go's 1.23 tarballs
were signed by a Google subkey that's gone from Google's current key, so
keep the key file that verified a release next to the manifest rather
than fetching it again.

`build` checks each file against the lock, like a package, and places the
files after every package. A file can add to a directory that's there,
like `/usr/local/bin`, but it can't replace anything a package installed,
or another file. Nothing checks that a binary's libraries are in the
image, so list them as packages: Node needs `libstdc++6` and
`libatomic1`.

### Releases from an index

Instead of a URL, a file can name a release line, and `lock` finds the
newest release in it from the publisher's index:

```toml
[files.go]                          # go.dev's list of Go releases
go        = "1.27"                  # the newest 1.27.x; "1.27.3" for just that
signed_by = "keys/google-linux.asc" # optional: check the tarball's .asc too
extract   = "go/"
path      = "/usr/local/go"

[files.etcdctl]                     # a public Google Cloud Storage bucket
gcs     = "etcd"                    # storage.googleapis.com/etcd
version = "3.6"
asset   = "etcd-v{version}-linux-{arch}.tar.gz"
extract = "etcd-v{version}-linux-{arch}/etcdctl"
path    = "/usr/local/bin/etcdctl"
```

`go` takes the Linux tarball for the image's architecture. `gcs` reads a
bucket with a directory per release named for its version (`v3.6.16/`),
optionally under a `prefix`, and takes `asset` from it. In `asset` and
`extract`, `{version}` is the release's version without its `v`, and
`{arch}` is the architecture as Go and OCI name it (`amd64`, `arm64`,
...). Prereleases never match.

The lock records the release as `version`. Nothing signs a bucket's
`SHA256SUMS` or go.dev's list, so a hash read from either is recorded as
`trust = "index"`: whoever controls the index vouches for it. A `go`
file with `signed_by` is checked against its `.asc` as well, and recorded
as `signed`.

A later `lock` keeps the release a file has while the manifest still
allows it and the index still has it, so an unrelated change doesn't
move it; a release the index no longer has moves to the newest. `lock
--upgrade` moves every file from an index to the newest release its line
allows, and `--json` shows what moved. A bucket's newest release has to
have `asset` for the image's architecture, or `lock` fails rather than
fall back to an older one. Go publishes no tarball for `armel`, whose
CPUs can be older than its ARM builds need.

dpkg doesn't know about files, so scanners that read its database, like
trivy and syft, won't see them. [`docs/files.md`](docs/files.md) has what
comes next: GitHub's releases.

## ACCOUNTS

A package that needs a user makes it in its `postinst`, with `adduser
--system`, which takes the next free ID. Which one that is depends on
what was configured before it, so adding a package to an image can move
`postgres` from 100 to 101, and files it owns on a volume stop being its.
Declare the users and groups instead, with fixed IDs:

```toml
[accounts]
groups = { ssl-cert = 101 }

[accounts.users.postgres]
uid    = 999                        # its own group, postgres, gets 999 too
groups = ["ssl-cert"]               # more groups it's a member of
home   = "/var/lib/postgresql"      # default /nonexistent
shell  = "/bin/bash"                # default /usr/sbin/nologin
gecos  = "PostgreSQL administrator"
```

A user's primary group is `group`, by default a group named after it. A
group named after the user is made with the user's UID as its GID, unless
`[accounts.groups]` or base-passwd has it already. Every image of the
manifest gets the accounts.

`build` writes `/etc/passwd` and `/etc/group` with base-passwd's entries
and then these, so the image needs base-passwd. Nothing can take a name
or an ID base-passwd has: its `users` group is GID 100, for one. In a
configured image the accounts are simply there. In an unconfigured one,
base-passwd's `postinst` keeps the files it finds, and so does the
`adduser --system` in a package's `postinst` when the user it asks for
exists: `postgresql-common` keeps `postgres` at 999, and `ssl-cert` keeps
its group at 101. Passwords are `*`, as base-passwd writes its own, until
configuring the passwd package moves them to `/etc/shadow`.

Accounts shape the image rather than what's in it, so like `entrypoint`
they're left out of the config hash, and changing them doesn't make the
lock stale.

## PATHS

Files, directories and symlinks can be written straight from the
manifest, with an owner and a mode:

```toml
[paths."/usr/sbin/policy-rc.d"]          # a file: start no services
content = """
#!/bin/sh
exit 101
"""
mode = "0755"                             # default 0644

[paths."/etc/postgresql-common/createcluster.d/mpg.conf"]
content = "create_main_cluster = false\n"

[paths."/var/lib/postgresql"]             # a directory
owner = "postgres"                        # default root
mode  = "0700"                            # default 0755

[paths."/run/postgresql"]
owner = "postgres"
group = "postgres"                        # default: the owner's group
mode  = "2775"

[paths."/usr/local/bin/python"]           # a symlink
target = "python3"
```

A path with `content` is a file, one with `target` is a symlink, and one
with neither is a directory. `owner` and `group` take a name or a number.
Names come from the image's `/etc/passwd` and `/etc/group`, which are
base-passwd's and the [accounts](#accounts), so a path can be owned by a
user the manifest declares.

Paths are written last, after the packages, the accounts and the
[files](#files), in manifest order, and every image of the manifest gets
them. A directory takes the owner and mode the manifest gives it, whether
a package made it or not, and its missing parents are made as root, 0755.
A file or a symlink can't replace anything that's there. Parents are
resolved through the image's symlinks, as a package's paths are, so
`/bin/x` lands in `/usr/bin` on a merged `/usr`. Like accounts, paths are
left out of the config hash.

## LOCALES

glibc's locales are compiled by `localedef`, which a package's
`postinst` would run in the image. Debian also ships every locale glibc
supports already compiled, in `locales-all`, and an image takes the ones
it asks for from there, without running anything:

```toml
[image]                                  # or [defaults], or [images.<id>]
locales = ["en_US.UTF-8", "pt_BR.UTF-8"]
```

Asking for locales adds `locales-all` to the image's packages, so it's
resolved and locked like any other, from the same glibc as `libc6`.
`build` then keeps the locales asked for in `/usr/lib/locale` and drops
the rest of `locales-all`'s, about 225 MB of them. Locales share files
through symlinks; a kept one's links into a dropped one become copies of
what they pointed at, so `en_US.UTF-8` alone takes about 3 MB. Names are
spelled as for `LANG` and found the way glibc finds them, so
`en_US.UTF-8` is `en_US.utf8` and `de_DE.ISO-8859-15@euro` is
`de_DE.iso885915@euro`. A locale `locales-all` doesn't have is an error
that lists the ones it has for that language. `C.UTF-8` comes with
libc-bin and is always there. `locales = ["all"]` keeps every locale.

dpkg's file list for `locales-all` still names the dropped files, so
`dpkg --verify` reports them missing, as it does for a `path-exclude`.
Set `LANG` in `entrypoint.env` for processes to pick a locale up. Which
locales an image keeps isn't in the config hash, only `locales-all`
coming in with the first one, so changing the list doesn't make the lock
stale.

## LOCK

`paranoid lock` fetches each repository's `InRelease` and checks its
signature against the `signed_by` keys. It then fetches the `Packages`
indexes, checks each against the SHA-256 the signed Release lists for it,
resolves the manifest, and writes `paranoid.lock` next to it:

```toml
config_hash = "sha256:4daa62e2..."

[[repository]]
name      = "debian"
date      = "2026-07-11T10:16:37Z"
inrelease = "77737fa4b34f2693e982cc9ee35736816c35a7778fc2d326cc1bbf5b301fe1aa"
signed_by = ["B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8"]

[[package]]
name         = "libc6"
version      = "2.36-9+deb12u14"
architecture = "arm64"
sha256       = "01f4330719fd4f65580e16ea5a0527f372fca750e8f588d26deaf09f2d3b1cf4"
filename     = "pool/main/g/glibc/libc6_2.36-9+deb12u14_arm64.deb"
size         = 2324660
repository   = "debian"

[[image]]
name     = "default"
packages = ["gcc-12-base", "libbrotli1", "libc-ares2", "libc6", ...]
direct   = ["nodejs"]

[[image.dep]]
parent   = "nodejs"
children = ["libc6", "libnode108"]
```

Each package is listed once. Each `[[image]]` names the packages it
contains, the ones the manifest asked for, and its dependency graph. A
single-image manifest has one image, `default`; a set has one per image.

Each `[[repository]]` records the snapshot the packages came from: the
Release's date, the SHA-256 of the `InRelease` file as served, and the keys
whose signatures checked out. That digest names the snapshot, so you can
find the same `InRelease` on a mirror archive such as snapshot.debian.org
and check it yourself.

The signature check follows what apt does with `gpgv`. One good signature
from a key in `signed_by` is required. A signature from one of those keys
that doesn't verify is an error, while signatures from other keys are
ignored. Revoked or expired keys, SHA-1 signatures and an expired
`Valid-Until` are refused (for a `snapshot`, expired at the snapshot's
time). paranoid is stricter than apt in two places:

- A repository without `InRelease` is refused. paranoid doesn't fall back
  to an unsigned `Release`.
- A validly signed Release for another distribution is refused, where apt
  only warns. That is what a downgrade to an older snapshot looks like.

`config_hash` is a SHA-256 over the canonical JSON of the manifest. If the
manifest changes, `lock` re-resolves and prints what was added, changed or
removed. It also re-resolves if none of the keys that signed the locked
snapshot are in `signed_by` any more. Otherwise it does nothing unless you
pass `--force`. Commit the lock.

If a dependency can't be satisfied from the repositories, `lock` lists
what's missing and writes nothing. `--allow-unmet` writes the lock anyway
and records each unmet dependency in it as an `[[unmet]]` table, so the
gap shows up in review. Every later `lock` or `build` on that lock needs
the same flag.

`paranoid diff OLD NEW` shows what changed between two locks, as `lock`
does when it re-resolves. With `--json`, both print the changes to stdout
in a format paranoid keeps stable across releases, whatever happens to the
lock's own layout. Read this rather than the lock:

```json
{
  "changed": true,
  "images": { "added": [], "removed": [] },
  "packages": [
    {
      "name": "libssl3",
      "from": "3.0.15-1~deb12u1",
      "to": "3.0.16-1~deb12u1",
      "repository": "debian-security",
      "images": ["server", "builder"]
    }
  ],
  "files": [
    {
      "name": "etcdctl",
      "from": { "url": "https://.../etcd-v3.6.15-linux-arm64.tar.gz", "sha256": "567d9b8f...", "version": "v3.6.15" },
      "to": { "url": "https://.../etcd-v3.6.16-linux-arm64.tar.gz", "sha256": "ad756ff2...", "version": "v3.6.16" },
      "images": ["server"]
    }
  ]
}
```

`from` is null for a package that's new and `to` for one that's gone.
`repository` is where the package comes from, or came from if it's gone,
and `images` are the images it changed in. A package that moves into or out
of an image at the same version counts as a change. A file changes when
its URL or hash does, and its `from` or `to` is null the same way; each
has the release's `version` when the file comes from an index.
`changed` is false when no package, file or image moved, even if the
snapshots did. Later releases may add fields, but won't remove any or
change what they mean.

## BUILD

```
  lock ─▶ fetch ─▶ verify ─▶ unpack ─▶ rootfs ─▶ layer ─▶ image
           │        sha256    data.tar  in memory  streamed OCI
           └─ cache  <user cache dir>/paranoid/sha256/xx/<hash>.deb
                     (~/.cache on Linux, ~/Library/Caches on macOS)
```

- Every `.deb` is checked against the lock's SHA-256, whether it came off
  the network or out of the cache. If a cached file doesn't match, the
  build stops; `--force-download` fetches it again.
- Packages are unpacked in lock order into an in-memory tree. Nothing is
  written to the host filesystem except the cache and the final image, so a
  hostile package can't escape through symlinks or `..`. Symlinks inside
  the image are resolved like dpkg does, and are confined to the image root.
- The image gets the dpkg database a real install would have:
  `/var/lib/dpkg/status` and `info/` with each package's file list,
  md5sums, conffiles and maintainer scripts. `dpkg -l`, `dpkg --verify` and
  vulnerability scanners see every package. `scripts/dpkg-db-oracle.sh`
  checks it against the database dpkg writes for the same packages.
- Ownership, modes and link targets come from the packages. Every mtime is
  clamped and the image `created` time comes from `SOURCE_DATE_EPOCH`
  (default: the epoch).
- The layer is streamed from the tree through gzip into a temporary file
  next to the output, so a build needs about as much memory as the
  unpacked image. The compressor is fed fixed-size chunks, which keeps its
  output independent of how the tar writes fall.
- Output goes to `-o`, else to the manifest's `output` (relative to the
  manifest), else to `image.tar`. A path ending in `.tar` gets a tarball
  for `podman load` / `docker load`, anything else an OCI image layout
  directory. Either is written next to its destination and renamed into
  place, so a failed build leaves the previous image as it was. A layout
  replaces the previous one whole, stale blobs included, and only ever
  replaces a layout or an empty directory.
- A stale lock stops the build before anything is downloaded. That means
  the manifest changed since the lock was written, or none of the keys that
  signed it are in `signed_by` any more. `--allow-stale` builds from it
  anyway.

paranoid never runs maintainer scripts (`preinst`, `postinst`, ...). By
default every package is marked installed anyway, so packages that rely on
their `postinst` to finish setting up (creating users, generating config,
running `update-alternatives`) won't be fully set up.

With `unconfigured = true` under `[image]`, the image is left the way `dpkg
--unpack` leaves a system: every package unpacked, its maintainer scripts in
`/var/lib/dpkg/info`, and its conffiles waiting at `<path>.dpkg-new`. dpkg
can then configure everything inside the image, the way debootstrap's
second stage does, for instance in a Dockerfile `RUN`:

```sh
printf '#!/bin/sh\nexit 101\n' >/usr/sbin/policy-rc.d   # start no services
chmod 755 /usr/sbin/policy-rc.d
dpkg --configure --force-depends base-passwd           # users and groups first
dpkg --configure --pending --force-configure-any --force-depends
test -z "$(dpkg --audit)"
rm /usr/sbin/policy-rc.d
```

Essential packages depend on each other in cycles, hence the `--force`
flags; debootstrap uses them too. The scripts then run in the image, as
root, the same as with apt. The image needs whatever they call (a shell,
Perl for debconf), and since nothing pulls Essential packages in, you
list them in the manifest.

## CACHE

`build` keeps every package and file it fetches in the local cache, under
its SHA-256:

```
<user cache dir>/paranoid/sha256/<first 2 hex>/<sha256>.deb    a package
<user cache dir>/paranoid/sha256/<first 2 hex>/<sha256>        a file
```

A store is the same layout behind a URL: a bucket, or any HTTP server.
`build` asks the stores listed under `[download]` for each package before
its repository, in order:

```toml
[download]
stores = ["https://debs.example.org/paranoid"]
```

A store is addressed by hash rather than by pool path, so one store serves
every repository and every lock. `mirrors` stand in for one repository
only, and a snapshot repository's URL changes with every snapshot.

Asking a store first is safe only because of the hash check. A store can
be stale, wrong or tampered with, and the worst it can do is fail the
build; it can't change an image. In exchange, an outage upstream never
reaches a package the store has. A store that doesn't have a package, or
is down, falls through to the next store and then to the repository, the
same as a mirror, and a store that serves the wrong bytes stops the build.
The summary at the end of a build says how many packages came from a
store. Like mirrors, stores play no part in resolution and are left out of
the config hash, so adding one doesn't make the lock stale.

Stores are read with plain GETs, without credentials. A store must answer
a 404 for a package it doesn't have: one that answers 200 with an error
page, as some CDNs and website-hosting setups do, serves the wrong bytes
and stops every build. Store, repository and mirror URLs can't carry a
query, a fragment or credentials, since package paths are appended to
them and they show up in error messages.

`paranoid cache push` fills a store with the lock's packages and files it
doesn't have yet. It takes them from the local cache, or fetches them the way
`build` would:

```
$ paranoid cache push https://debs.example.org/paranoid
Pushed 12 packages (34.1 MB) to https://debs.example.org/paranoid; 330 were there already.
```

Without a URL it pushes to the manifest's store, if it lists exactly one.
Each package gets a `HEAD`, and a `PUT` if the store doesn't have it or
has it at the wrong size, so pushing also repairs a truncated upload. A
403 to the `HEAD` counts as missing too, since S3 answers that for a
missing key unless you may list the bucket; if you can't write either,
the `PUT` says so. Packages to push are fetched the way `build` would,
except from the store being filled. With `AWS_ACCESS_KEY_ID` and
`AWS_SECRET_ACCESS_KEY` set (and `AWS_REGION`, by default `us-east-1`),
requests are signed for S3, so the URL can be an S3-compatible bucket's,
and the bucket checks each upload against the package's SHA-256. Every
package is checked against the lock before it's pushed. An existing
directory instead of a URL gets the cache's layout, to sync with other
tools.

A typical setup is a bucket anyone can read, a `stores` line in the
manifest, and `paranoid cache push` after each build on the main branch,
which is the only place that can write to the bucket.

## GUARANTEES

What holds today:

```
  same lock + same SOURCE_DATE_EPOCH      ─▶  same image digest
  package lists                           ─▶  signed by a key you chose
  package and file contents               ─▶  checked against the lock
  host filesystem                         ─▶  untouched by package contents
  file ownership in the image             ─▶  taken from the package, or
                                              root for a fetched file
```

What does **not** hold yet:

```
  rechecking a lock         build trusts the checksums in the lock. Keep the
                            lock under review the way you would any other
                            pinned dependency.
  complete resolution       the resolver is greedy, with restarts. It picks
                            what apt picks for the packages in
                            scripts/apt-oracle.sh, but contrived conflicts
                            can fail where a full search would succeed.
```

See [`docs/next.md`](docs/next.md) for what's next.

## INSPECT

```sh
$ paranoid inspect lock --tree         # dependency tree from the lock
$ paranoid inspect image image.tar     # name, arch, entrypoint, size, digest
```

## HACKING

```sh
$ cargo test                    # unit + offline integration tests
$ cargo test -- --ignored       # network tests: real mirrors, real builds,
                                #   including build-twice-and-compare
$ cargo clippy --all-targets
$ scripts/apt-oracle.sh         # compare resolution with apt (needs podman);
                                #   BACKPORTS=default|<n> adds backports
$ scripts/dpkg-oracle.sh        # regenerate dpkg's version-order test data
$ scripts/dpkg-db-oracle.sh     # compare /var/lib/dpkg with dpkg's own
$ scripts/trust-oracle.sh       # break a signed repo ten ways, ask apt too
```

```
src/
  config/      manifest parsing and validation
  repository/  InRelease signatures, Packages indexes, versions,
               relations
  resolve/     manifest ─▶ resolved package set
  lock/        lock file format and diffing
  download/    parallel fetch + content-addressed cache
  deb/         ar reader, data.tar extraction
  rootfs.rs    in-memory filesystem tree
  oci/         layers, config, manifest, layout and tarball writers
  build.rs     the build pipeline, library side
  report.rs    progress events (Phase, Event, Reporter)
  cli/         one module per subcommand, each a wrapper over the library
```

## INSTALL

Binaries for Linux (static, amd64 and arm64) and macOS (arm64 and x86_64)
are on the [releases page](https://github.com/jwnx/paranoid/releases), with
a `SHA256SUMS` file and build provenance you can check with
`gh attestation verify <archive> -R jwnx/paranoid`. Or build from source
with `cargo install --path .`.

## STATUS

paranoid is early in development. The lock format and the library API will
change.

## LICENSE

Apache License 2.0. See [`LICENSE`](LICENSE).

```
                                                                  PARANOID(1)
```
