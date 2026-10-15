# What's next

The October 2026 tidy-up fixed image building: the rootfs is assembled in
memory, builds are reproducible, extraction can't escape, and there is one
progress model, one hash type and a thin `main.rs`. Resolution and trust
were left alone on purpose. They are next, in this order, because each one
changes what the lock file contains.

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

## 2. Trust chain

The tool is called paranoid, but nothing anchors trust today. The SHA-256
values in the lock come from `Packages` files fetched without checking
`InRelease`, over mirrors that may be plain HTTP. `fetch_release` exists
but is never called.

1. Fetch `InRelease` and verify its signature against a configured keyring
   (`sequoia-openpgp`, or shell out to `gpgv`).
2. Parse its `SHA256:` table and verify each `Packages` file before
   parsing it.
3. Record the Release digest in the lock, so `build` can show which signed
   snapshot a lock came from.

## 3. Policy

- `lock` writes a lock even with unmet dependencies, and `build` builds
  from a stale lock with only a warning. Both should fail by default, with
  `--allow-unmet` / `--allow-stale` to opt out.

## 4. Smaller cleanups

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
- `paranoid inspect lock | head` panics on a broken pipe.
- `examples/etcd` asks for a package named `etcd`, which bookworm doesn't
  have (it's `etcd-server`).
