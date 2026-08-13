# Termux packaging fixture

This directory is a development fixture for the upstream project. It is not
an official Termux recipe or an installation channel.

The canonical recipe can exist only at
`termux/termux-packages/packages/termux-stacks/build.sh`. Before copying this
fixture there, replace every placeholder, select the SQLite strategy, add the
tested removal scripts, and validate all four Termux architectures.

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
