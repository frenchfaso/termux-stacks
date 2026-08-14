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

## S4 — Interrupted install ownership

S4 requires the S2/S3 engine baseline, Python, and a reviewed external arm64
OCI archive built from `fixtures/s4/Containerfile`. The second layer contains
50,000 empty files: it remains small when compressed and makes the interval
after the public `Applying layer 2/2` marker observable on the device.

After placing the digest-pinned Alpine base in the local image cache, build
and save the fixture off-device with arm64 Podman/Buildah:

```bash
podman build --platform linux/arm64 --format oci --pull=never \
  --network=none --no-cache --layers=false --timestamp 0 \
  --tag localhost/termux-stacks-s4-fixture:v1 \
  --file tests/device/fixtures/s4/Containerfile \
  tests/device/fixtures/s4
podman save --format oci-archive \
  --output termux-stacks-s4-fixture-linux-arm64.oci.tar \
  localhost/termux-stacks-s4-fixture:v1
sha256sum termux-stacks-s4-fixture-linux-arm64.oci.tar
tar -xOf termux-stacks-s4-fixture-linux-arm64.oci.tar index.json \
  | jq -r '.manifests[0].digest'
```

The reviewed v1 manifest digest is pinned by
`BLESSED_MANIFEST_SHA256` in `fixtures/s4/verify-oci.sh`. Any fixture rebuild
must be reviewed and must update that trust root before an acceptance run.
The external archive remains outside the repository and is supplied with its
exact SHA-256:

```bash
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/s4.sh \
  --archive "$HOME/termux-stacks-s4-fixture-linux-arm64.oci.tar" \
  --archive-sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --output-root "$HOME/termux-stacks-evidence"
```

S4 rejects output roots outside Termux's canonical app-private `files`
directory. The default acceptance matrix is C0, three F1 cycles, and three F2
cycles:

1. C0 completes a local OCI install and qualifies the positive observer;
2. F1 kills the exact installer scope after a loopback server has sent and
   fsynced its first-chunk download barrier;
3. F2 stops and kills the exact installer scope after unbuffered public stderr
   announces application of OCI layer 2/2 and before any later public phase
   marker appears.

`--fault-cycles 1..10` is available for diagnostics; the default is three.
Every attempt uses a fresh synthetic `TERMUX__PREFIX`, a random alias from
`/dev/urandom`, a durable intent entry, and a qualified boot-ID/PID/start-time/
PGID/SID scope. The harness never mutates the real runtime, reuses an alias,
or uses `--all`, `reset`, `clear-cache`, a glob, or a pre-existing target.

After invocation, `owned` requires two successful, stderr-free public
`list --quiet` observations containing exactly the one expected alias. An
empty list is never interpreted as `absent`; any failed, empty, changing, or
malformed observation is `ambiguous`. Only `owned` permits an exact public
`remove ALIAS`, followed by two empty public inventories and deletion of the
pinned private sandbox. `ambiguous` fails the run and preserves the sandbox.

The evidence bundle includes raw installer/server/list output, an fsynced
intent ledger, process identities, `golden.tsv`, and `preserved.tsv`. For a
questionable artifact, do not rerun the alias and do not broaden the target.
Archive the evidence, confirm that every recorded process scope drained, and
review the two raw public observations. If ownership still cannot be proven,
retain the synthetic sandbox; a reviewer may later remove that exact private
sandbox only after independently checking its sentinel and that no recorded
scope remains. S4 has no SQLite, daemon, workload, or production parser.

## S5 — Single-service vertical slice

S5 runs the real debug daemon, SQLite store, protocol, adapter, foreground
supervisor, and public `proot-distro 5.6.0` CLI in fresh synthetic prefixes.
It requires native aarch64 Termux, the exact blessed Alpine OCI archive
accepted by `fixtures/s5/verify-oci.sh`, and a debug binary built from the
source under test:

```bash
cargo build --locked
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/s5.sh \
  --binary "$PWD/target/debug/termux-stacks" \
  --archive "$HOME/termux-stacks-s5-alpine-arm64.oci.tar" \
  --archive-sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --output-root "$HOME/termux-stacks-evidence"
```

The default acceptance run uses 20 post-start crash cycles. A smaller
`--unknown-cycles 1..20` value is diagnostic only and does not complete S5.
The matrix covers:

1. normal `up/status/down`, stopped-rootfs reuse, daemon SIGTERM, cold restart,
   exact request replay, and exact protocol-version rejection;
2. cooperative and TERM-ignoring root/child/grandchild trees stopped through
   the exact engine session ID;
3. a real `SQLITE_FULL` result on the daemon connection, rollback, integrity
   check, and a lifecycle after restoring capacity;
4. controlled daemon death before intent, after intent, after install, after
   start, before commit, and during down;
5. 20 after-start cold recoveries that must become `unknown` without an
   automatic engine effect or duplicate workload.

The checkpoints and SQLite page limit are debug-only test interfaces. Release
binaries omit them and cannot run this harness. Ambiguous process identity,
database state, containment, or cleanup fails the run and preserves the exact
private sandbox. The harness never broadens a target, uses an alias as a kill
target, or mutates the user's real PRoot runtime. Raw bundles remain outside
the repository; the reviewed acceptance summary is in
[`docs/evidence/S5.md`](../../docs/evidence/S5.md).
