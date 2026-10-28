# Configuring images

Status: phase 1 (configuring through podman or Docker) is implemented.
The rest is a proposal.

## Why

paranoid unpacks packages and never runs their maintainer scripts. That
leaves two kinds of image:

- **Installed** (the default): every package is marked installed, but
  nothing a `postinst` does has happened. There's no CA bundle, no
  alternatives (`awk`, `editor`), no PAM configuration, and conffiles are
  where the package put them.
- **Unconfigured** (`unconfigured = true`): the image is the way `dpkg
  --unpack` leaves a system, and something else runs `dpkg --configure`
  in it later.

Real images need the scripts. The MPG bases are unconfigured, and every
Dockerfile that uses them has a `RUN configure.sh <role>` stage with three
bind mounts: the second stage of debootstrap, written by hand. That script
has grown its own policy-rc.d, its own user creation and ID checks, its
own `localedef`, its own apt sources read out of `paranoid.toml` with
`sed`, and its own list of files to delete so the result is reproducible.

Most of that is now declarative: `[accounts]`, `[paths]`, `locales` and
`apt_sources`. What's left is running dpkg itself. This proposal adds
that to paranoid, so `build` produces a finished, reproducible image and
configure.sh, its roles and the wrapper stages go away.

## What it does

```toml
[images.pg17]
configure = true       # instead of unconfigured = true
```

`build` assembles the rootfs as today, writes the accounts, locales,
files, apt sources and paths, and then:

1. Writes `/usr/sbin/policy-rc.d` (`exit 101`) so no service starts.
2. Runs, in a sandbox (see below), with stdin from `/dev/null`, no
   network, `DEBIAN_FRONTEND=noninteractive`, `LC_ALL=C.UTF-8`, `TZ=UTC`
   and the build's `SOURCE_DATE_EPOCH`:

   ```sh
   dpkg --configure --force-depends base-passwd
   dpkg --configure --force-depends --force-configure-any <required>
   dpkg --configure --pending --force-depends --force-configure-any
   dpkg --audit                      # anything printed is an error
   ```

   `<required>` is every package still unpacked with `Priority: required`
   or `Essential: yes`, in lock order: the order configure.sh uses, and
   debootstrap before it. Pending triggers (`ldconfig`, `man-db`) run as
   part of `--pending`.
3. Reads the resulting filesystem back into the rootfs.
4. Checks that what the manifest declared is still so (see "Declarations
   win"), and cleans up (see "Reproducibility").

The image then has every package `installed`, the way apt would leave it,
and the same lock and paranoid version give the same digest.

`configure` and `unconfigured` can't both be set. The default stays as it
is, so existing images don't change.

### No hooks

There's no `run = [...]`. Every step configure.sh takes outside dpkg has a
declarative form now:

| configure.sh | paranoid |
|---|---|
| `policy-rc.d` while configuring | written and removed by `configure` |
| `createcluster.d/mpg.conf` | `[paths]` |
| `useradd postgres` 999:999 before configuring | `[accounts]` |
| check that postinst gave postgres 100:102 | declare 100:102 in `[accounts]` |
| `localedef en_US.UTF-8` | `locales = ["en_US.UTF-8"]` |
| `install -d -m 1777 /var/lib/postgresql/data` | `[paths]` |
| apt sources and the PGDG pin | `apt_sources = true` |
| `rm` of logs, `*-old`, aux-cache, snakeoil | `configure`'s cleanup |

Commands would make paranoid a build system, and "same lock, same image"
would depend on whatever they do. An image that needs more can still run
it in a Dockerfile on top.

Two gaps to check first: configure.sh's `apt.conf.d` (retries and
timeouts, a global `Check-Valid-Until`) has no equivalent, though
`[paths]` can write it; and its `localedef` runs with `-c -A
locale.alias`, so its `en_US.UTF-8` should be compared with the one
`locales-all` ships before the bases switch.

### Declarations win

A `postinst` can undo a declaration: `install -d -m 755` over a directory
`[paths]` made 0700, say. So after configuring:

- every directory in `[paths]` gets its declared owner and mode again;
- every file and symlink in `[paths]` has to be unchanged, or it's an
  error naming the path;
- every user and group in `[accounts]` has to still have its declared
  IDs, or it's an error. `adduser --system` keeps an existing user, so
  this only fails when a script does something unusual, which is what
  configure.sh's 100:102 check is for.

Paths that must exist before configuring, like `createcluster.d`, are
written before it, as now. Only directories are re-applied after.

## The sandbox

The scripts run as root, from packages the lock pins and a key signed.
That's the same trust as `apt install`. The sandbox is there so they can't
touch the host and can't see anything that varies between builds.

### Phase 1: an OCI runtime

paranoid hands the assembled rootfs to the container runtime the build
machine already has, `podman` or `docker`, picked with `--runtime` or
found on `PATH`:

1. Write the rootfs as an image (the layer we'd write anyway) and load
   it.
2. `run --rm --network=none --pull=never -i` the steps above, with `/dev/null`
   as stdin.
3. `export` the container's filesystem and read it into a fresh rootfs.

This works wherever the Dockerfile `RUN` works today: rootless podman,
Docker on CI, podman machine on macOS. Another architecture runs through
the runtime's emulation (binfmt with qemu-user-static), as `docker buildx`
does now. Emulation is slow, but no slower than the current workflow,
which does the same.

Ownership survives because the runtime maps root and every system ID, and
`export` writes them as they are.

### Phase 2: a native sandbox on Linux

Without a runtime: unshare user, mount, PID, network, IPC and UTS
namespaces, map IDs with `newuidmap`/`newgidmap` from the user's
`/etc/subuid` range (as rootless podman does) or run as real root, mount
the rootfs with a fresh `/proc`, `/dev` (null, zero, urandom, tty) and an
empty `/tmp`, `chroot`, and run the same steps. Reading the result back is
a directory walk.

This removes a dependency and a layer round trip, but brings the hard
parts a runtime already solves: ID mapping for unprivileged users,
seccomp, emulation for other architectures. It's worth it only once
phase 1 shows where the time goes.

macOS has no namespaces, so there the runtime stays the only way.

## Reproducibility

Configuring is the first step in which anything varies by itself. What we
know varies, from configure.sh and from Debian's reproducible builds work:

| What | Why | What we do |
|---|---|---|
| `/var/log/dpkg.log`, `/var/log/alternatives.log`, `/var/log/apt/` | timestamps | remove |
| `/var/lib/dpkg/*-old`, `/var/cache/debconf/*-old` | backups dpkg and debconf make | remove |
| `/var/cache/ldconfig/aux-cache` | inode numbers and times | remove; `ldconfig` rebuilds it |
| `/etc/ssl/private/ssl-cert-snakeoil.key`, `/etc/ssl/certs/ssl-cert-snakeoil.pem` | a random key pair | remove |
| `/etc/shadow`, `/etc/gshadow` | the day of last change is today | set it to `SOURCE_DATE_EPOCH`'s day |
| `/usr/sbin/policy-rc.d` | ours | remove |
| `/etc/machine-id`, `/var/lib/dbus/machine-id` | random, if anything makes one | remove |
| file times | when the scripts ran | already: layers carry no times |

`*.pyc` files carry their source's time, which the rootfs fixes, so they
come out the same; `py_compile`'s hash-based mode would make sure.

That list will never be complete, so paranoid also checks:
`build --check-reproducible` configures twice, from the same rootfs, and
lists every path whose content, mode or owner differs. The MPG bases are
the first test: their configure.sh cleanup is where the list above
started.

## Lock and cache

`configure` changes the image, not what's in it, so it stays out of the
config hash, as `unconfigured` does. The image's digest depends on the
lock, the manifest and the paranoid version, as now. Expected digests in
the lock (TODO item 3) would make "same lock, same image" checkable here
too, and `--check-reproducible` is how to find out when it isn't.

A configured rootfs could be cached by the hash of its inputs, so a
rebuild that only changes `[paths]` skips configuring. That's an
optimization for later.

## Phases

1. **Configure through an OCI runtime:** `configure = true`, the dpkg
   steps, declarations re-checked after, the cleanup list, and
   `--check-reproducible`. Proven on the MPG bases by building them with
   `configure = true` and comparing with today's configure.sh output: the
   same files, owners and modes, apart from what this proposal changes on
   purpose (the shadow date).
2. **A native Linux sandbox**, if phase 1 shows the runtime round trip is
   what's slow.
3. **A configured-rootfs cache**, if configuring is what's slow.

## Open questions

- Should `configure = true` become the default? It's what most images
  want, but it needs a runtime where none was needed, and it changes
  every existing image's digest. Probably not until phase 2 makes it
  dependency-free on Linux.
- Should the configure output be a layer of its own over the unpacked
  rootfs? It would share the big layer between images that differ only in
  configuration, but the diff would include every file a trigger rewrote
  (ldconfig's cache, for one), so the saving may be small.
- debconf answers: some packages ask questions (tzdata, locales). With
  `noninteractive` they take defaults. Does any MPG base need a
  preseeded answer? If so, a declarative `[debconf]` table, not a hook.
