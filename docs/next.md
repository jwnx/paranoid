# What's next

The October 2026 tidy-up fixed image building: the rootfs is assembled in
memory, builds are reproducible, extraction can't escape, and there is one
progress model, one hash type and a thin `main.rs`. Resolution and trust
were left alone on purpose. They are next, in this order, because each one
changes what the lock file contains.

## 1. Resolver correctness

The resolver gives plausible answers for simple manifests but is wrong at
the edges. Each item below was reproduced during the review.

- **Malformed relations silently become "no dependencies"**
  (`package.rs:130-148`). `(>=2.17)` without a space drops the constraint,
  `(>=  2.17)` keeps a leading space, and an unknown arch restriction fails
  the whole list, which is then swallowed. Parse failures should be errors,
  and the three relation parsers should share one operator table.
- **Virtual packages are chosen wrong** (`package_index.rs:187-276`):
  - an unversioned `Provides` satisfies a versioned dependency;
  - providers with no priority sort first;
  - a virtual provider beats a real package with the same name;
  - `arch: all` providers are never found (`debconf-2.0` resolves to nothing).
- **No global consistency** (`resolver.rs`). Each root is resolved alone
  and merged by name, so when `A` needs `libfoo (<< 2)` and `B` needs
  `libfoo`, one constraint silently loses. Alternatives only try the first
  option, `Breaks`/`Conflicts` are ignored, and `with_recommends` does
  nothing because `Recommends` is never parsed. Start with a single pass
  that checks already-selected packages against each new constraint and
  reports conflicts; reach for `pubgrub` or `resolvo` if that isn't enough.
- **Repository pins only relabel** (`resolve/service.rs`). A package pinned
  to a repository is still resolved from all of them, so its download URL
  can point at the wrong mirror. Pass the pin into resolution.

Once these land, make `PackageSpec.version` a parsed constraint in the
manifest (it is still parsed late, in the resolver), and give
`ResolvedManifest.unmet` a typed `Vec<UnmetDependency>` instead of strings.

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
- Single-implementation traits (`Version`, `Package`) and the
  `ProvidesList`/`DependencyList` `Deref<Target = Vec>` wrappers can become
  concrete types.
- Most modules are `pub`. Shrink the library surface to what the CLI and
  tests need.
- The HTTP client has no timeouts, and decompresses `Packages` into a
  `String`, so one non-UTF-8 `Description` fails a whole repository.
- `paranoid inspect lock | head` panics on a broken pipe.
- `examples/etcd` asks for a package named `etcd`, which bookworm doesn't
  have (it's `etcd-server`).
