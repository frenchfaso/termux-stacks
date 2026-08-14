# Device harness

The harnesses collect evidence for gates run on a real Termux installation.
S0 tests the binary, package, and runit integration; S1 qualifies the public
semantics of `proot-distro run`. They are separate from the production
adapters and do not require the Termux Stacks daemon, except for the isolated
scaffold test in S0.

## S0 — Bootstrap

This harness collects reproducible evidence for the S0 checkpoint on a Termux
device. It verifies an **already provided** `termux-stacks` binary: it does not
compile, install packages, run `apt`/`pkg`, enable services, or use `sudo`.

## Prerequisites

- Termux with Bash and coreutils;
- a `termux-stacks` binary executable for the device architecture;
- `PREFIX` set by the Termux session;
- `file` and `readelf` are optional: the corresponding checks become `SKIP`
  when the tool is unavailable.

The `termux-stacks` package and runit integration are not prerequisites. Their
checks are read-only and become `SKIP` if the package is not installed.

## Usage

Minimal invocation:

```bash
bash tests/device/s0.sh --binary /path/to/binary/termux-stacks
```

To select the **base** evidence directory:

```bash
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/s0.sh \
  --binary /path/to/binary/termux-stacks \
  --output-root "$HOME/termux-stacks-evidence"
```

`--output-root` must name an absolute, existing, writable directory. Without
this option, `TMPDIR` is used. In either case, the script uses `mktemp` to
create a private, unique `termux-stacks-s0.*` directory; it neither reuses nor
overwrites a previous directory.

The daemon runs with a synthetic `PREFIX` under the harness's private
workspace. The test does not create `state.db`, locks, or sockets under the
device's real `$PREFIX/var`. TERM and KILL signals are sent only to child
processes started by the script.

## Checks

S0 covers:

1. an inventory of Termux, Android, the architecture, filesystem, and relevant
   packages;
2. `termux-stacks --version` and `--help`;
3. the binary's SHA-256 and, when available, `file` and `readelf`;
4. creation of private paths under the synthetic prefix and their modes;
5. exclusion of a second daemon through the lock/socket;
6. lock release and stale-socket recovery after TERM;
7. lock release and stale-socket recovery after KILL;
8. read-only inspection of the package and runit service, if installed.

The `s0.sh` script contains no S1–S4 tests, OCI images, or `proot-distro`
operations.

## Evidence

The directory printed at completion contains:

```text
evidence/
├── metadata.tsv
├── results.tsv
├── stdout-stderr/
├── conclusions.md
└── SHA256SUMS
```

`results.tsv` uses the `PASS`, `FAIL`, and `SKIP` states. A `FAIL` produces exit
code `1`; only `PASS`/`SKIP` results produce exit code `0`. `conclusions.md`
contains an automatic summary and space for manual review. The harness keeps
`evidence/` and removes only its own `work/` subtree.

## S1 — Entrypoint and Cmd

S1 currently requires an aarch64 Termux device, `proot-distro 5.6.0`, the
standard core Termux tools, and the `alpine:3.24.1` image already visible in
the local cache. The preflight deliberately avoids requesting a missing
image, but the quiet inventory does not prove the architecture or completeness
of the cache and does not certify an offline build. The harness builds four
local fixtures: Entrypoint+Cmd, Cmd only, Entrypoint only, and neither:

```bash
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/s1.sh --output-root "$HOME/termux-stacks-evidence"
```

Each run generates random `txs-s1-*` tags and aliases, records their intent
before creation, and uses exact-name comparisons only. Teardown checks the
baseline through public interfaces, stops/removes only its own aliases, and
removes only its own image references. It never uses `clear-cache`,
`reset`, `remove --all`, globs, or a pre-existing alias. If the inventory
becomes ambiguous, it fails without broadening the cleanup target.

The matrix verifies defaults and overrides for all four shapes, problematic
argv values in hexadecimal form, the working directory, environment, exit
status, and the `login` shell boundary. It does not test the session registry
or signals; those belong to S2 and S3.

## S2 — Session registry

S2 requires an aarch64 Termux device with `proot-distro 5.6.0`, Bash, `jq`,
`flock`, `setsid`, and the standard coreutils. It receives an external OCI
archive and its SHA-256; before installation it verifies the archive,
manifest, config, and every layer, including the `linux/arm64` platform and
`/s2-worker` Entrypoint:

```bash
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/s2.sh \
  --archive "$HOME/termux-stacks-s2-worker-linux-arm64.oci.tar" \
  --archive-sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --output-root "$HOME/termux-stacks-evidence"
```

The fixture is built off-device with arm64 Podman/Buildah. The `Containerfile`
pins the Alpine base by digest; the blessed archive is produced with
`podman save --format oci-archive` and remains outside the repository. The
checksum qualifies that artifact; it does not promise a byte-identical tar
across different Podman versions.

All engine mutations use synthetic `TERMUX__PREFIX` and `TERMUX__HOME` values
inside the run's private directory, plus `PD_PROOT_BIN` pointing to the real
`proot`. The preflight must show the synthetic data location, and the
synthetic runtime must be empty before installation. A random exact-name alias
is recorded before the effect; the real runtime is checked only with
`list --quiet` to rule out a collision. The harness does not use
`clear-cache`, `reset`, global targets, or user aliases; imported rootfs
instances and layers are discarded by removing only the sandbox after all
owned processes have drained.

The suite covers:

1. T1, positive visibility and pruning after normal exit;
2. T2, two independent sessions on the same alias;
3. F1, denied registration;
4. F2, denied and restored registry reads;
5. F3, a locked record truncated on the same inode.

The oracle uses the foreground child together with boot ID, PID, start time,
PGID, and SID; it is not a production parser. S2 does not test tree-kill,
tracer loss, or guest signals: those cases belong to S3. Loss of the real
daemon and reconciliation belong to the S5 vertical slice.

## S3 — Signals and tree-kill

S3 requires the same aarch64 device and engine baseline as S2. It receives an
external blessed OCI archive and verifies the platform, manifest, config,
layers, base, and worker bytes before installing it into its own synthetic
`TERMUX__PREFIX`:

```bash
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/s3.sh \
  --archive "$HOME/termux-stacks-s3-worker-linux-arm64.oci.tar" \
  --archive-sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --output-root "$HOME/termux-stacks-evidence"
```

The acceptance run uses the default of 100 cycles. `--stress-cycles 0..100`
is only for diagnostic runs; a result with fewer than 100 cycles does not
complete S3.

The suite tests a cooperative tree, a tree that ignores TERM, a grandchild in
a new session, two sessions on the same alias, and guests left alive after the
PRoot tracer receives SIGKILL. The only candidate production target is
`proot-distro kill SESSION_PID`. Direct TERM to the PGID is a negative
control; aliases, `--all`, and host PIDs are not fallbacks.

Drain is PASS only when known roles, record holders, and all qualified
PGIDs/SIDs are empty. In an ambiguous case, the harness waits for the TTL,
fails, and preserves the sandbox; it never broadens the cleanup target. Guest
events and identities are copied into the evidence before the rootfs is
removed.
