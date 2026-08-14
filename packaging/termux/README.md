# Termux packaging fixture

This directory is a development fixture for the upstream project. It is not
an official Termux recipe or an installation channel.

The canonical recipe can exist only at
`termux/termux-packages/packages/termux-stacks/build.sh`. Before copying this
fixture there, update the release fields, add the source checksum, exercise
the removal script, and validate all four Termux architectures. The selected
SQLite strategy is dynamic linking against the packaged `libsqlite`. The
engine dependency is pinned to `proot-distro 5.6.0` because the runtime
capability probe accepts exactly that qualified version.

The fixture's `prerm` acts only on an actual package removal. It disables and
stops the fixed `termux-stacksd` runit service and removes that package-owned
service directory. It deliberately does nothing during an upgrade, so an
upgrade neither restarts the daemon nor changes runtime state. The state
database, logs, volumes and PRoot root filesystems are never package-owned
removal targets. For Debian output the generated guard accepts only the
`remove` action. For Pacman output the Termux builder converts the same script
to `pre_remove`, so the generated body is unconditional and cannot run during
an upgrade.

## G3 canonical build

Use a clean, commit-pinned `termux-packages` clone and a fresh package-builder
container. Create the immutable upstream tag before materializing the checksum
in the external recipe, hash the exact GitHub tag archive, and never move the
tag. A release candidate should use Debian pre-release ordering in the recipe,
for example `0.1.0~rc.1`; the source URL maps that value to the SemVer tag
`v0.1.0-rc.1`.

Stage the materialized fixture as
`packages/termux-stacks/build.sh`, then run:

```bash
CONTAINER_NAME=termux-package-builder-termux-stacks \
  ./scripts/run-docker.sh \
  ./build-package.sh -a all -f -r -I termux-stacks
```

Accept the outputs only after all four architectures are present, the package
metadata and file list are exact, the service is disabled by default, and each
ELF dependency is provided by a declared runtime package. The release gate
must also inspect the binary for test hooks and run the install, live-upgrade,
disabled-upgrade, and removal matrix on Android.

## S0 on-device fallback

The S0 aarch64 gate used the official `termux-packages` pipeline natively on
Termux because the amd64 package-builder image was not usable on the available
ARM virtual machine. This is a development fallback, not the release or
official-repository build path.

Stage only these two files in a clean, commit-pinned `termux-packages` clone:

```text
packages/termux-stacks/build.sh
sources/termux-stacks-${VERSION}.tar.gz
```

The staged recipe may differ from this fixture only in `TERMUX_PKG_SRCURL`
and `TERMUX_PKG_SHA256`. The source must be an archive of the clean upstream
commit under test. Verify its checksum and paths before building. Do not copy
`target/`, `.git/`, Cargo registries, build caches, previous output or core
dumps into staging.

After explicitly verifying `rust`, `cargo`, `termux-services`,
`termux-elf-cleaner` and Termux's execution environment, the minimal S0 command
is:

```bash
./build-package.sh -f -s termux-stacks
```

`-s` skips dependency resolution. It is valid here only because the complete
recipe dependency set was checked first; it is not for CI, releases or an
official package proposal. An on-device build installs into the live
`$PREFIX` before collecting changed files. Therefore the temporary recipe
must override the collector with an allowlist containing only the binary,
license and `termux-stacksd` service files, and the resulting `.deb` must be
inspected before installation. The canonical off-device builder uses its
isolated prefix and the default collector.

The same constrained native path was repeated for the S5 checkpoint from an
immutable source archive. Its temporary recipe added the now-required
`libsqlite` and `proot-distro (>= 5.6.0)` dependencies; the resulting package
and stripped release ELF are recorded in
[`docs/evidence/S5.md`](../../docs/evidence/S5.md). This does not replace the
four-architecture package gate or materialize the placeholders in
`build.sh.fixture`.
