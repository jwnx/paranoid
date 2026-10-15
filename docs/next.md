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

## 3. Smaller cleanups

- `oci/spec.rs` hand-writes about 400 lines of serde. Use derives
  (`rename_all`, `skip_serializing_if`), or the `oci-spec` crate.
- `OciConfig` has paired setters (`env`/`try_env`, `workdir`/`try_workdir`,
  `image_name`/`try_image_name`, …). Keep only the typed ones and let callers
  parse at the edge.
- `PackageIndex` keys by `(Architecture, String)` and allocates on every
  lookup. A per-arch `HashMap<String, BTreeMap<DebianVersion, _>>` gives
  `latest_version` for free. `VirtualPackageProvider` clones whole packages.
- The single-implementation `Version` trait can become the concrete
  `DebianVersion`.
- Most modules are `pub`. Shrink the library surface to what the CLI and
  tests need.
- The HTTP client has no timeouts, and decompresses `Packages` into a
  `String`, so one non-UTF-8 `Description` fails a whole repository.
