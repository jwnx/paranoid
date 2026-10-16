# What's next

The October 2026 tidy-up fixed image building: the rootfs is assembled in
memory, builds are reproducible, extraction can't escape, and there is one
progress model, one hash type and a thin `main.rs`. Resolution and trust
came next, since each changed what the lock file contains, and then the
policy for unmet dependencies and stale locks. All three are done apart
from the notes below.

## 1. Resolver

Version comparison, relation parsing and package selection were rewritten
after the tidy-up. `scripts/dpkg-oracle.sh` and `scripts/apt-oracle.sh`
check them against dpkg and apt. What's left:

- The resolver is greedy, with restarts when a requirement needs a
  different version of something already chosen (see `resolver.rs`). If a
  real manifest hits a conflict apt gets around, reach for `pubgrub` or
  `resolvo`.
- Essential packages (`dpkg`, `coreutils`, ...) are never declared as
  dependencies, so an image only has them if the manifest lists them. apt
  does the same on an empty system, but it's easy to trip over: apt itself
  needs dpkg's data files to run. Consider a warning, or an opt-in.

- On bookworm, an `unconfigured` image that has `usrmerge` (it satisfies
  `init-system-helpers`' dependency on merged `/usr`) fails to configure
  it: `usrmerge`'s postinst refuses to convert under overlayfs. debootstrap
  avoids it by creating the merged-`/usr` links before installing anything.
  Consider doing the same for distributions that expect it, or picking
  `usr-is-merged` when the links are there.

## 2. Trust chain

Every repository names its keys with `signed_by`. `lock` verifies
`InRelease` against them (`keyring.rs`), checks each `Packages` index
against the signed Release (`release.rs`), and records the snapshot in the
lock. `scripts/trust-oracle.sh` breaks a signed repository ten ways and
checks paranoid against apt. What's left:

- Only `InRelease` is read. Some third-party repositories publish only a
  `Release` with a detached `Release.gpg`. Support that if one is needed.
- Designated revokers aren't honored. Debian's archive keys name several,
  and a revocation they issue only counts once the revoker's key is in the
  keyring. gpgv behaves the same, but apt users get revocations through
  keyring package updates and paranoid users don't.
- `build` trusts the checksums in the lock. A `paranoid verify` could fetch
  the recorded `InRelease` back from a snapshot archive, check it against
  the keys, and check the lock against it.
- An unknown signer is reported by the key ID or fingerprint in the
  signature, which for Debian is a subkey; the primary fingerprint would be
  friendlier, but we can't know it without the key.

## 3. Several images from one resolution

A manifest describes one image and a lock records one resolution. Real
users build families of images from the same repositories, and paranoid
gives them no way to say so.

The first user is the MPG data plane (superfly/mpg, `build/paranoid` on
the `jwnx-exp-paranoid-dataplane-images` worktree). It builds seven bases:
the pg16 and pg17 runtimes, each with and without PostGIS, a builder per
PostgreSQL major that compiles extensions for the matching runtime, and
the pgBackRest repository server. All seven use the same five repositories
(three suites of a snapshot.debian.org snapshot, PGDG, and PGDG's
apt-archive at priority 100), the same keys and the same 28 essential
packages. Today a Python script writes the seven manifests, a shell script
runs `lock` and `build` in each directory, and the same Python script
checks the seven locks against each other afterwards.

That works, but it shows three gaps:

- **Duplication.** Repositories, keys and package sets are repeated in
  every manifest, because there is no include or inheritance.
- **Agreement between images.** Some packages must be identical across
  images, and only a check outside paranoid enforces it:
  - pgBackRest must be the same version on the repository host and on
    every database host. It is held (`version = "..."`), and the hold is
    repeated in five manifests.
  - Extensions are built against the builder's
    `postgresql-server-dev-17` and loaded by the runtime's
    `postgresql-17`. They are different binary packages from the same
    source package, and must be the same build.
- **Resolving at different times.** This is the real bug. Each lock
  fetches and verifies every repository's `InRelease` on its own. For
  snapshot.debian.org that's harmless, since a snapshot never changes. PGDG
  has no snapshots, though. If PGDG publishes 17.12 between locking
  `pg17-builder` and locking `pg17`, the builder gets 17.12 headers and the
  runtime gets 17.11, and nothing in paranoid notices. The external check
  catches it after the fact. Seven locks also verify the same five
  repositories seven times.

Until paranoid handles this, the external check can detect the timing bug
exactly. Every lock records the SHA-256 of each repository's `InRelease`
in its `[[repository]]` table. If all seven locks show the same `inrelease`
for `pgdg` and `pgdg-archive`, they resolved against the same state of
PGDG. If any differ, re-lock them.

What a fix needs to provide:

- One set of repositories fetched and verified once per lock, so every
  image resolves against the same snapshot of each repository, recorded
  once.
- A way to share repositories, keys and package lists between images
  without copying them.
- Holds that apply across images, defined once.
- Each image still gets exactly its own dependency closure: the builder's
  compilers must not leak into a runtime.
- A clear answer when two images need incompatible versions of the same
  package. For example, Cargo's unified lock allows one version per package
  across the workspace and reports a conflict instead of choosing.
- `build` of a single image, and of all of them.
- Arguably, a generic form of the server/server-dev rule: binary packages
  built from the same source package should come from the same *source
  version* across the set. That's the version in `Source: name (version)`,
  or the package's own version when the field has none. The binary
  versions themselves can't be compared: a binNMU rebuilds some binaries
  without a new source and gives them a `+bN` suffix, so in bookworm main
  1,794 of 34,171 sources ship binaries at different versions
  (`libcaf-dev` is `0.17.6-2.1+b1`, `libcaf-doc` from the same source is
  `0.17.6-2.1`). The rule holds naturally when everything resolves from
  one snapshot, but stating it turns a silent mismatch (say, from a hold
  on only one of the binaries) into an error.

Questions to settle:

- Is this one manifest with several `[images.*]` tables, or several
  manifests that include a shared one? Includes solve the duplication but
  not the timing; one lock for the set solves both.
- Is the unit of locking the whole set, or can one image be re-locked
  alone? With PGDG moving, re-locking alone reintroduces the timing bug.
- How does the config hash and staleness work when one image's package list
  changes? Is the whole lock stale, or only that image?
- What does `inspect lock` show, and what does the lock diff look like for
  review, when one change touches several images?
- Per-image settings (`unconfigured`, entrypoint, name, architecture): can
  images in one set differ in architecture? MPG only needs amd64 today.

Out of scope: what the MPG images do after paranoid builds them. Creating
the postgres user with a fixed uid, `create_main_cluster = false` and the
locale are set up by MPG's own `configure.sh`, which runs `dpkg --configure`
in a Dockerfile, and that stays there.

## 4. Smaller cleanups

- `oci/spec.rs` hand-writes about 400 lines of serde. Use derives
  (`rename_all`, `skip_serializing_if`), or the `oci-spec` crate.
- `OciConfig` has paired setters (`env`/`try_env`, `workdir`/`try_workdir`,
  `image_name`/`try_image_name`, …). Keep only the typed ones and let callers
  parse at the edge.
- `PackageIndex` keys by `(Architecture, String)` and allocates on every
  lookup. A per-arch `HashMap<String, BTreeMap<DebianVersion, _>>` gives
  `latest_version` for free.
- The single-implementation `Version` trait can become the concrete
  `DebianVersion`.
- Most modules are `pub`. Shrink the library surface to what the CLI and
  tests need.
- The HTTP client has no timeouts, and decompresses `Packages` into a
  `String`, so one non-UTF-8 `Description` fails a whole repository.
