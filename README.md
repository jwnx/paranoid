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
paranoid lock    [-f MANIFEST] [-o LOCK] [--force] [--allow-unmet]
paranoid build   [-f MANIFEST] [--lock LOCK] [-o OUT] [--image ID]...
                 [--cache-dir DIR] [--jobs N] [--force-download]
                 [--allow-stale] [--allow-unmet]
paranoid cache push    [URL|DIR] [-f MANIFEST] [--lock LOCK] [--cache-dir DIR]
                       [--jobs N]
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
paranoid     = "~0.3"            # optional, the paranoid versions to use

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

`build` tries each source up to three times when it fails with a 5xx, a
timeout or a dropped connection, half a second apart and then a second,
and then moves on to the next. Any other failure, such as a 404 or a 403,
moves on at once. A source still failing after its retries is skipped for
the rest of the build wherever a package has somewhere else to go, so an
outage costs one package its retries rather than every package. A source that serves bytes that don't match the lock
stops the build there, without trying the rest, since it's broken or
tampered with. When no source has a package, the error lists what each
one answered.

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

`build` keeps every package it fetches in the local cache, under its
SHA-256:

```
<user cache dir>/paranoid/sha256/<first 2 hex>/<sha256>.deb
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

Stores are read with plain GETs, without credentials.

`paranoid cache push` fills a store with the lock's packages it doesn't
have yet. It takes them from the local cache, or fetches them the way
`build` would:

```
$ paranoid cache push https://debs.example.org/paranoid
Pushed 12 packages (34.1 MB) to https://debs.example.org/paranoid; 330 were there already.
```

Without a URL it pushes to the manifest's store, if it lists exactly one.
Each package gets a `HEAD`, and a `PUT` if the store doesn't have it or
has it at the wrong size. With `AWS_ACCESS_KEY_ID` and
`AWS_SECRET_ACCESS_KEY` set (and `AWS_REGION`, by default `us-east-1`),
requests are signed for S3, so the URL can be an S3-compatible bucket's,
and the bucket checks each upload against the package's SHA-256. Every
package is checked against the lock before it's pushed. A directory
instead of a URL gets the cache's layout, to sync with other tools.

A typical setup is a bucket anyone can read, a `stores` line in the
manifest, and `paranoid cache push` after each build on the main branch,
which is the only place that can write to the bucket.

## GUARANTEES

What holds today:

```
  same lock + same SOURCE_DATE_EPOCH      ─▶  same image digest
  package lists                           ─▶  signed by a key you chose
  package contents                        ─▶  checked against the lock
  host filesystem                         ─▶  untouched by package contents
  file ownership in the image             ─▶  taken from the package
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
