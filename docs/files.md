# Files

Status: proposal. Phase 1 (pinned URLs) is being implemented.

## Why

Some of what goes into an image doesn't come from apt: the Go toolchain
from go.dev, etcd from its GitHub releases, a static binary a vendor
publishes. Today that means a Dockerfile on top of a paranoid image, with
its own `curl` and its own idea of what to trust, outside the lock.

paranoid can bring these in under the same rules as packages. A package is
trusted because a chain runs from a signing key to its bytes: a signed
Release lists the hash of `Packages`, which lists the hash of the `.deb`.
A file gets a chain too, however short. `lock` checks it once and records
the file's SHA-256 and size, and `build` checks the bytes against the lock.
Everything else then works for files as it does for packages: stores,
failover, refusing the wrong bytes, and the same image from the same lock.

## Not in scope

- **Running anything.** No commands, scripts or build steps. paranoid would
  become a build system, and "same lock, same image" would no longer hold.
  A file is bytes pinned by their hash, placed at a path.
- **Dependency resolution.** A file that needs libraries gets them from
  packages the manifest lists. See "Later" for a check.
- **Zip archives,** in the first phases. Every Linux release we've looked at
  ships a tarball.

## Manifest

Files are declared once, at the top level, under an id:

```toml
[files.etcd]
url     = "https://github.com/etcd-io/etcd/releases/download/v3.6.16/etcd-v3.6.16-linux-arm64.tar.gz"
sha256  = "ad756ff27ba21e3488cfb84d00f3f4ad8d9b60a93e20f5be5b230a4aab1dc053"
extract = "etcd-v3.6.16-linux-arm64/etcdctl"
path    = "/usr/local/bin/etcdctl"

[files.go]
url     = "https://go.dev/dl/go1.23.4.linux-arm64.tar.gz"
sha256  = "16e5017863a7f6071363782b1b8042eb12c6ca4f4cd71528b2123f0a1275b13e"
extract = "go/"
path    = "/usr/local/go"

[files.tini]
url    = "https://github.com/krallin/tini/releases/download/v0.19.0/tini-static-arm64"
sha256 = "eae1d3aa50c48fb23b8cbdf4e369d0910dfc538566bfd09df89a774aa84a48b9"
path   = "/usr/bin/tini"
mode   = "0755"
```

- `path` is where the file goes, absolute.
- Without `extract`, the download itself is the file. It gets `mode`,
  `0644` by default, and is owned by root.
- `extract = "dir/member"` takes one member of a tar archive (plain, gzip,
  xz or zstd, told apart by their first bytes). The member keeps its mode
  from the archive unless `mode` is set.
- `extract = "dir/"`, ending in `/`, takes everything under that directory
  and puts it under `path`, keeping the archive's modes, symlinks and hard
  links. `mode` isn't allowed here.
- Whatever the archive says, everything is owned by root, and the image's
  timestamps are the same as for packages.

A single-image manifest gets every file. In a set, each image lists the ids
it wants:

```toml
[images.etcd]
packages = ["ca-certificates"]
files    = ["etcd"]
```

Files are part of the config hash: changing one makes the lock stale, as
changing a package does. A manifest without files hashes exactly as it
does today, so existing locks stay current.

### Sources

How paranoid trusts a file depends on where it comes from. Each phase below
adds a kind of source. They differ in the keys that pick them:

| Source | Keys | What's trusted | Version bumps |
|---|---|---|---|
| Pinned URL | `url`, `sha256` | whoever wrote the manifest | edit the URL and hash |
| Signed URL | `url`, `signature` or `checksums`, `signed_by` | the signing key | edit the URL |
| Go | `go = "1.23"` | Google's signing key, via `.asc` | `lock --upgrade` |
| GitHub release | `github = "etcd-io/etcd"`, `version`, `asset` | GitHub's digest | `lock --upgrade` |

The lock records which kind each file came from (`trust`), so review and
`--json` show which files have no signature behind them.

## Lock

```toml
[[file]]
name   = "etcd"
url    = "https://github.com/etcd-io/etcd/releases/download/v3.6.16/etcd-v3.6.16-linux-arm64.tar.gz"
sha256 = "ad756ff27ba21e3488cfb84d00f3f4ad8d9b60a93e20f5be5b230a4aab1dc053"
size   = 23045869
trust  = "pinned"

[[image]]
name     = "default"
packages = [...]
files    = ["etcd"]
```

`lock` downloads each file, unless the cache already has it, checks it
against its source and records its size. That means the first `lock` of a
manifest with Go in it downloads about 70 MB. The download goes into the
cache, so the `build` that follows doesn't repeat it.

`path`, `mode` and `extract` stay in the manifest, covered by the config
hash, as an image's entrypoint is.

## Build

`build` downloads files with the same service as packages:

- the stores first, at `<store>/sha256/<2 hex>/<sha256>` (no `.deb`),
- then the file's own URL,
- with the same retries, failover, size limit and hash check.

Files are placed after every package, in manifest order. A file that would
replace something a package installed is an error, since that's almost
always a mistake and dpkg would never know. A directory that already
exists is fine: `/usr/local/bin` comes from `base-files`.

dpkg doesn't list files, so scanners that read its database (trivy, syft)
won't see them. The README says so. An SBOM is the eventual answer.

`paranoid cache push` pushes files along with packages.

## Diff

The JSON diff gets a `files` field. Adding a field is allowed by the
contract:

```json
"files": [
  {
    "name": "etcd",
    "from": { "url": "https://.../v3.6.15/...", "sha256": "..." },
    "to":   { "url": "https://.../v3.6.16/...", "sha256": "..." },
    "images": ["etcd"]
  }
]
```

`from` is null for a new file and `to` is null for one that's gone.
`changed` is true when a file moved, as it is for a package.

## Phases

1. **Pinned URLs:** `url` and `sha256`, `extract`, lock, build, stores,
   cache push and diff. This is what's being built now.
2. **Signed URLs:** a detached OpenPGP signature over the file
   (`signature`), or over a checksums file that lists it (`checksums`,
   the `SHA256SUMS` + `SHA256SUMS.asc` pattern), checked with `signed_by`
   at lock time. Uses the OpenPGP code we already have.
3. **Go:** resolve `go = "1.23"` against `go.dev/dl/?mode=json`, check the
   tarball's `.asc`, and `extract = "go/"` by default.
4. **GitHub releases:** resolve `version` against the Releases API and
   template `asset` with `{version}` and `{arch}` (Debian names mapped to
   the usual `amd64`, `arm64`, ...). The asset's `digest` is the only
   anchor, so the lock records `trust = "github"`. Uses `GITHUB_TOKEN` when
   set, since the API allows 60 unauthenticated requests per hour.

Phases 3 and 4 make files bumpable. `lock --upgrade` (or a name like it)
would move them to the newest version their constraint allows, the way
`--snapshot` moves repositories.

## Later

- **An ELF check.** Read a file's interpreter and `DT_NEEDED` and fail the
  build when the image doesn't have them. It's cheap and catches the most
  common mistake, a dynamically linked binary in an image without its libc.
- **A layer of its own** for large files like the Go toolchain, so they
  don't share a layer with the packages that change more often. Images are
  one layer today.
- **`env`** on images, to put `/usr/local/go/bin` on `PATH`.
