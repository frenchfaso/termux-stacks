# Termux packaging

**Status:** pre-release path, not an official recipe
**Sources verified:** 2026-08-14

## 1. Outcome

Termux Stacks is technically suitable for a non-root Termux package in the
`main` channel, but a new project does not yet automatically meet the
acceptance criteria. The realistic path is:

1. upstream build and on-device tests;
2. public releases and real users;
3. optionally TUR;
4. package request;
5. recipe in `termux/termux-packages/packages/termux-stacks/`.

Only the recipe inside `termux/termux-packages` is canonical. An upstream
`packaging/termux/build.sh.fixture` is a test/template and must clearly say so.

## 2. Candidate blockers

The official policy requires, among other things, an active/well-known project,
a recognized open-source license, a package under 100 MiB, and no duplication.
It also states that software normally installable through a language package
manager, including `cargo`, should use that channel.

Before submitting a proposal, the project therefore needs:

- the Apache-2.0 license and a `LICENSE` file;
- immutable tags/releases and source archives;
- demonstrable community adoption and maintenance;
- a clear distinction from `proot-distro` and `docker-compose`;
- no instructions promoting `cargo install` as the user installation channel;
- a rationale for the Termux-specific integration: prefix, runit,
  `proot-distro`, Bionic, and the Android lifecycle;
- maintainer feedback on using “Termux” in the name;
- builds and tests on all four architectures;
- measured package size.

The greatest acceptance risk is perceived maturity/duplication, not Rust. A
correct recipe does not guarantee inclusion.

## 3. Upstream layout

```text
termux-stacks/
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── src/
├── tests/
├── docs/
└── packaging/termux/
    ├── README.md
    └── build.sh.fixture
```

Requirements:

- one Cargo package, one binary, `publish = false`;
- committed `Cargo.lock`;
- `rust-version = 1.93`; the official Rust package was 1.97.1 when verified;
- build from a source archive without a Git repository;
- no hardcoded FHS paths;
- a configurable prefix in tests;
- no dependency downloads or self-updates during post-install, startup, or
  runtime; `up` may explicitly acquire an OCI image;
- no crate shipped as an opaque binary.

The builder may download toolchains/crates as allowed by the Termux
infrastructure. `termux_setup_rust` itself installs the toolchain during the
cross-build, so the project must not promise a fully offline build unless it
deliberately introduces vendoring.

## 4. Experimental recipe

Skeleton to complete after the tag, license, and dependency spikes:

```bash
TERMUX_PKG_HOMEPAGE=https://github.com/frenchfaso/termux-stacks
TERMUX_PKG_DESCRIPTION="Declarative service stacks for Termux using PRoot"
TERMUX_PKG_LICENSE="Apache-2.0"
TERMUX_PKG_MAINTAINER="@frenchfaso"
TERMUX_PKG_VERSION="<version>"
TERMUX_PKG_SRCURL="https://github.com/frenchfaso/termux-stacks/archive/refs/tags/v${TERMUX_PKG_VERSION}.tar.gz"
TERMUX_PKG_SHA256="<sha256>"
TERMUX_PKG_DEPENDS="libsqlite, proot-distro (>= 5.6.0), termux-services"
TERMUX_PKG_BUILD_IN_SRC=true
TERMUX_PKG_SERVICE_SCRIPT=(
  "termux-stacksd" 'exec "$PREFIX/bin/termux-stacks" daemon 2>&1'
)

termux_step_pre_configure() {
  termux_setup_rust
}
```

Notes:

- version/tag and checksum remain placeholders until a release exists;
- S5 adds `proot-distro (>= 5.6.0)` and the system `libsqlite`; the package
  build must still inspect the ELF to prove that SQLite is not bundled;
- if a crate enables bundled SQLite, the decision requires an ADR and CVE
  audit;
- the standard Cargo build/install path in `termux-packages` uses `--locked`
  with configured targets and jobs; the single-package structure avoids custom
  selection;
- do not install a `termux-stacksd` binary.

The official service-script helper creates the `down` file, so the installed
service is disabled. The user command is `sv-enable termux-stacksd`.

## 5. Installed layout

```text
$PREFIX/
├── bin/termux-stacks
├── var/service/termux-stacksd/
│   ├── run
│   ├── down
│   └── log/run
├── var/lib/termux-stacks/       # created at runtime
└── var/run/termux-stacks/       # ephemeral
    ├── daemon.lock
    └── daemon.sock
```

Rules:

- the database, volumes, and logs are not package-owned files and survive
  ordinary upgrades/removal;
- the lock file and socket are recreated; the authoritative lock is the kernel
  lock, not the file contents;
- no durable state lives in `$HOME` or shared storage;
- the runtime does not read internals under `$PREFIX/var/lib/proot-distro`;
- a future explicit purge must show its targets and require consent.

## 6. Service, installation, and upgrade

Runit executes the daemon in the foreground. Stderr is redirected into the
service logging stream. Post-install neither enables nor starts the service and
does not perform long migrations.

After the first installation of `termux-services`, the user must restart the
shell so that `service-daemon` starts. This is a documented operational
prerequisite, not behavior controlled by Termux Stacks.

After an upgrade, the old process may continue executing the already mapped
ELF. The CLI and daemon include an exact protocol version:

- compatible: proceed;
- incompatible: fail fast with `sv restart termux-stacksd`;
- no automatic restart from post-install.

The first supported format transition is the transactional schema-2 to
schema-3 migration introduced by M1. It performs no engine effect, preserves
an unknown future schema unchanged, and maps any inherited active or
incomplete state to `unknown`. The package upgrade matrix must exercise this
migration with both disabled and running daemon scenarios; package maintainer
scripts do not run it.

Removal is distinct from upgrade. The package-candidate `prerm`, only during
actual removal:

1. creates the fixed `down` file and requests a bounded, exact `sv down`;
2. on Debian, aborts removal unless the supervised daemon is proven stopped;
3. prevents runit from retrying an executable that has been removed;
4. does not delete the database, volumes, logs, or rootfs.

The G3 package-candidate gate qualifies the Debian lifecycle, including an
enabled service, a disabled service, and upgrade. Pacman/libalpm executes the
converted `pre_remove` hook but does not abort removal when that hook exits
nonzero, so it cannot inherit Debian's fail-closed removal guarantee. The
fixture must retain a bounded best-effort Pacman stop without deleting state,
but Pacman lifecycle qualification remains an explicit blocker for G4.

## 7. CI and device tests

Upstream, for every change:

- `cargo fmt --all -- --check`;
- `cargo clippy --all-targets --locked -- -D warnings`;
- `cargo test --locked`;
- fake-engine tests and manifest fixtures.

After selecting the native crates:

- build the package for `aarch64`, `arm`, `i686`, and `x86_64`;
- verify `Cargo.lock`;
- audit licenses/advisories;
- use `readelf` or equivalent to inspect NEEDED entries and declared
  dependencies;
- measure `.deb` and installed sizes;
- perform a clean build using the intended Termux CI modes;
- install the `.deb` in a clean Termux environment, with dependencies resolved
  by the package manager.

On at least one aarch64 device:

1. install the package;
2. verify that `termux-stacksd/down` exists;
3. enable the service and inspect logs/status;
4. run the engine spikes;
5. test upgrade with a live daemon;
6. test kill -9, controlled disk full, and recovery;
7. test removal with the service enabled/disabled and verify that data
   survives.

For the S0 gate only, a minimal native fallback is documented in
[`packaging/termux/README.md`](../packaging/termux/README.md). It skips
dependency resolution only after an explicit preflight and uses an allowlist
collector because the on-device builder operates on the real `$PREFIX`. It
does not replace the off-device build required for releases and the official
proposal.

Reboot, Doze, force-stop, and multiple devices become RC gates, not gates for
the first binary.

## 8. Termux:Boot

Termux:Boot is optional and is not a package dependency. The add-on must come
from a source/signature compatible with the Termux app, be opened at least
once, and receive an executable script under `~/.termux/boot/`. The recommended
configuration has that script start the `termux-services` infrastructure;
runit decides which services without a `down` file to start.

Termux:Boot is a one-shot launcher, not a watchdog. After Android force-stops
the app, the user must reopen Termux. Wake lock and boot are explicit user
choices.

## 9. Official proposal checklist

- [ ] name approved or changed before stable paths;
- [x] SPDX license Apache-2.0;
- [ ] active project, releases, and users;
- [ ] immutable source archive/tag/checksum;
- [ ] distinction from the engine and Compose documented;
- [ ] no promotion of `cargo install`;
- [ ] four-architecture build;
- [ ] package under 100 MiB with margin;
- [ ] service disabled by default;
- [ ] upgrade and removal preserve state;
- [ ] maintainer available;
- [ ] package request recommended before the PR;
- [ ] official recipe maintained only in `termux-packages`.

## 10. Sources

- [Packaging policy](https://github.com/termux/termux-packages/blob/master/CONTRIBUTING.md#packaging-policy)
- [Creating a package](https://github.com/termux/termux-packages/wiki/Creating-new-package)
- [Building packages](https://github.com/termux/termux-packages/wiki/Building-packages)
- [Rust helper](https://github.com/termux/termux-packages/blob/master/scripts/build/setup/termux_setup_rust.sh)
- [Service-script installation](https://github.com/termux/termux-packages/blob/master/scripts/build/termux_step_install_service_scripts.sh)
- [termux-services](https://github.com/termux/termux-services)
- [proot-distro recipe](https://github.com/termux/termux-packages/blob/master/packages/proot-distro/build.sh)
- [PRoot-Distro v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/README.md)
