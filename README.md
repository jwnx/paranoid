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
manager. You list the packages you need in `paranoid.toml`. `paranoid lock`
resolves their dependencies against the repositories you list and writes every
package's exact version and SHA-256 to `paranoid.lock`. `paranoid build`
downloads those packages, checks them against the lock, unpacks them and
writes the image. Building the same lock twice gives the same image digest.

The images it makes are in the style of
[distroless](https://github.com/GoogleContainerTools/distroless): your
application and its runtime libraries, and nothing else unless you add it.
If you want a shell in the image, add one, like `bash`, to the package list.

```
  paranoid.toml ──lock──▶ paranoid.lock ──build──▶ image.tar
```

## SYNOPSIS

```
paranoid lock    [-f MANIFEST] [-o LOCK] [--force]
paranoid build   [-f MANIFEST] [--lock LOCK] [-o OUT] [--cache-dir DIR]
                 [--jobs N] [--force-download]
paranoid inspect lock  [FILE] [--tree]
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
[image]
name         = "node"            # optional, also the title label
tag          = "latest"          # optional, defaults to latest
architecture = "arm64"           # required; amd64, arm64, armhf, ...
description  = "..."             # optional, OCI label
authors      = "..."             # optional, OCI label
source       = "..."             # optional, OCI label

[repositories.debian]            # order matters: on a tie, first repo wins
url          = "https://deb.debian.org/debian"
distribution = "bookworm"
components   = ["main"]

[[packages]]
name       = "nodejs"
version    = ">= 18"             # optional: "1.2-3" or "(>=|<=|>>|<<|=) 1.2"
repository = "debian"            # optional pin to one repository

[entrypoint]
cmd     = ["node"]               # becomes the image ENTRYPOINT
workdir = "/app"
env     = { NODE_ENV = "production" }
```

`cmd` is the image's *entrypoint*: arguments to `podman run` are appended
to it, they don't replace it.

Architectures take Debian names or the usual aliases (`x86_64`, `aarch64`,
`ppc64le`, ...) and map to the right OCI platform, variants included
(`armel` → `arm/v5`, `armhf` → `arm/v7`). `all` and `any` are not real
targets and are rejected.

## LOCK

`paranoid lock` resolves the manifest against the repositories' `Packages`
indexes and writes `paranoid.lock` next to it:

```toml
config_hash = "sha256:13b382ba..."

[[package]]
name         = "libc6"
version      = "2.36-9+deb12u14"
architecture = "arm64"
sha256       = "01f4330719fd4f65580e16ea5a0527f372fca750e8f588d26deaf09f2d3b1cf4"
filename     = "pool/main/g/glibc/libc6_2.36-9+deb12u14_arm64.deb"
size         = 2324660
repository   = "debian"
direct       = false
```

`config_hash` is a SHA-256 over the canonical JSON of the manifest. If the
manifest changes, `lock` re-resolves and prints what was added, changed or
removed; if not, it does nothing unless you pass `--force`. Commit the lock.

## BUILD

```
  lock ─▶ fetch ─▶ verify ─▶ unpack ─▶ rootfs ─▶ layer ─▶ image
           │        sha256    data.tar  in memory  tar+gz   OCI
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
- Ownership, modes and link targets come from the packages. Every mtime is
  clamped and the image `created` time comes from `SOURCE_DATE_EPOCH`
  (default: the epoch).
- Output is a tarball for `podman load` / `docker load` if `-o` ends in
  `.tar`, otherwise an OCI image layout directory.

Maintainer scripts (`preinst`, `postinst`, ...) are never run. Packages
that rely on their `postinst` to finish setting themselves up (creating
users, generating config, running `update-alternatives`) won't be fully set
up in the image, and you'll need to do that work some other way.

## GUARANTEES

What holds today:

```
  same lock + same SOURCE_DATE_EPOCH      ─▶  same image digest
  package contents                        ─▶  checked against the lock
  host filesystem                         ─▶  untouched by package contents
  file ownership in the image             ─▶  taken from the package
```

What does **not** hold yet:

```
  repository signatures     InRelease is not verified. The checksums in the
                            lock are only as trustworthy as the mirror you
                            locked against. Use https mirrors.
  dpkg-exact resolution     version ordering, Provides and conflicting
                            constraints differ from apt at the edges.
```

Both are the next things to fix. See [`docs/next.md`](docs/next.md).

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
```

```
src/
  config/      manifest parsing and validation
  repository/  Debian Packages indexes, versions, relations
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

## STATUS

paranoid is early in development. The lock format and the library API will
change, especially once signature checking lands and the lock starts
recording which signed Release it came from.

```
                                                                  PARANOID(1)
```
