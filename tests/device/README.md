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

## G2 — Multi-service MVP

G2 is the Android acceptance harness for M1. It uses the real debug daemon,
v3 SQLite store, public `proot-distro 5.6.0` adapter, and two reviewed arm64
OCI fixtures. It never uses the device's real engine state for a workload:
every case has a fresh synthetic `TERMUX__PREFIX` and `TERMUX__HOME` below a
private `mktemp` directory. The harness derives the canonical Termux `files`
tree from `$PREFIX` and rejects an output root or `$TMPDIR` outside that
application-private tree.

### Device prerequisites

- native aarch64 Termux;
- Bash, Python, Git, `jq`, `proot-distro 5.6.0`, `proot`, and the standard
  coreutils (`sha256sum`, `sync`, `sort`, `cmp`, `find`, `stat`);
- a clean Git checkout and a debug `termux-stacks` binary built from that
  source; a dirty or untracked source tree is rejected;
- the reviewed revision-3 v1 and v2 arm64 OCI archives and their archive
  SHA-256 values;
- enough free app-private storage for several disposable Alpine rootfs
  generations. No package installation is performed by the harness.

The output root and `$TMPDIR` must be real directories under the same canonical
Termux application-private `files` tree. Shared storage is rejected even when
it appears writable. The fault checkpoints are compiled only in debug builds.
The restart-cap test uses one initial start and at most five retries with the
production candidate delays of 1, 2, 4, 8, and 16 seconds, so a complete run
takes longer than an ordinary smoke test.

### Building the external fixtures

The fixture is generic and has no application-specific dependency. Its Alpine
base is pinned by digest in
`fixtures/g2/Containerfile`. Build each version off-device from the same
reviewed worker, without network access after the base is present:

```bash
for version in v1 v2; do
  podman build \
    --platform linux/arm64 \
    --format oci \
    --pull=never \
    --network=none \
    --no-cache \
    --layers=false \
    --timestamp 0 \
    --build-arg "G2_FIXTURE_VERSION=$version" \
    --tag "localhost/termux-stacks-g2-fixture:$version" \
    --file tests/device/fixtures/g2/Containerfile \
    tests/device/fixtures/g2
  podman save --format oci-archive \
    --output "termux-stacks-g2-fixture-$version-linux-arm64.oci.tar" \
    "localhost/termux-stacks-g2-fixture:$version"
  sha256sum "termux-stacks-g2-fixture-$version-linux-arm64.oci.tar"
  tar -xOf "termux-stacks-g2-fixture-$version-linux-arm64.oci.tar" index.json \
    | jq -r '.manifests[0].digest'
done
```

Revision 3 replaces an unavailable BusyBox `httpd` applet with the `nc`
applet already present in the pinned base. That changes the worker bytes and
invalidates both previous manifests:

```text
v1 superseded: sha256:e109d20537180d5b8d8d1f346a7573e2c417f502de7b590cd1df02a077744c5e
v2 superseded: sha256:0fa8687a5d0607ff25804c2e7a67da8439f4af2990868ecc29677ca0b0ceec77
```

Those values are historical evidence only and must never authorize a
revision-3 run. The two revision-3 manifest digests are reviewed and frozen
together as `BLESSED_MANIFEST_*_SHA256` values in `verify-oci.sh`. Whenever
the worker, Containerfile, or build contract changes, rebuild and review both
fixtures, replace both trust roots in the same commit, and leave the source
tree clean. Do not replace only one root and do not pass a manifest digest on
the harness command line.

The reviewed manifest digests are repository-owned acceptance trust roots. The
two archive hashes qualify the exact transferred tar serializations and remain
explicit harness inputs. `verify-oci.sh` verifies the frozen manifest for the
selected version, every referenced compressed blob, the `linux/arm64` config,
both decompressed layer diff IDs, the pinned base diff ID, fixture revision,
Entrypoint, version marker, and archived worker bytes against the repository
fixture. The reviewed revision-3 archive and manifest values are recorded in
`docs/evidence/G2.md`.
The archives and raw evidence remain outside Git.

### Acceptance command

```bash
cargo build --locked
mkdir -p "$HOME/termux-stacks-evidence"
test -z "$(git status --porcelain=v1 --untracked-files=all)"
bash tests/device/g2.sh \
  --binary "$PWD/target/debug/termux-stacks" \
  --archive-v1 "$HOME/termux-stacks-g2-fixture-v1-linux-arm64.oci.tar" \
  --archive-v1-sha256 V1_ARCHIVE_SHA256 \
  --archive-v2 "$HOME/termux-stacks-g2-fixture-v2-linux-arm64.oci.tar" \
  --archive-v2-sha256 V2_ARCHIVE_SHA256 \
  --output-root "$HOME/termux-stacks-evidence"
```

Replace both archive hash placeholders with exactly 64 lowercase hexadecimal
characters. Keep the checkout, archives, and evidence directory separate so
creating evidence cannot make the source tree dirty. A standard Termux shell
already sets app-private `$HOME` and `$TMPDIR`; the harness verifies both.

### Acceptance matrix

The normal lifecycle proves:

1. two simultaneous stacks, each with a stable `seed -> web` dependency and
   two independently owned foreground sessions;
2. literal environment values, manifest-relative binds, one private named
   volume per stack, reachable fixed loopback ports, and pre-effect rejection
   of a duplicate port declaration;
3. bounded two-stream logs and a per-service restart on each stack; each
   restart replaces only the selected web session, preserves its alias and
   rootfs generation, and leaves the peer service and peer stack identities
   byte-for-byte unchanged;
4. explicit `down`, then v1-to-v2 `up`, with new aliases, retained retired
   rootfs generations, unchanged volume data, and the second stack unaffected;
5. exact reverse-DAG stop order and an empty synthetic session registry.

The restart case executes one initial start and at most five retries. The
worker records a timestamp immediately before every planned normal failure.
The harness captures all five durable `next_restart_at` values and requires
both each failure-to-deadline interval and each failure-to-next-start interval
to meet the 1/2/4/8/16-second minimum. After the fifth retry fails, no further
start is permitted.

The controlled crash matrix covers:

- death between the first and second service starts: the first service becomes
  `unknown`, the untouched dependent remains `failed/absent`, and no effect is
  retried;
- death after both service starts succeed but before the parent `up` commits:
  parent-only cold recovery terminalizes the ambiguous parent, retains both
  successful child journals, marks both active services `unknown`, and creates
  no duplicate operation or engine effect;
- death after durable down intent but before the first engine kill: both
  sessions remain unchanged and recovery does not infer or repeat the stop;
- death after a failed process is proven absent and its retry is durably in
  backoff: cold recovery performs exactly the one authorized retry.

The first three ambiguous cases are cleaned only with the exact session IDs
captured before daemon death. A failed identity check, failed registry read,
unexpected session, or incomplete drain preserves the entire private runtime
and reports its path. Cleanup never uses an alias as a kill target, `--all`,
`reset`, `clear-cache`, a glob, or a pre-existing target. The real container
inventory is observed before and after and must remain byte-for-byte unchanged.

### Evidence and failure diagnostics

Preflight records the clean source commit, binary hash, `Cargo.toml` and
`Cargo.lock` hashes, harness/shared-library hashes, all fixture-source hashes,
archive/manifest identities, platform versions, and the app-private roots.
`SHA256SUMS` covers every resulting evidence file.

Before cleanup after any case failure, and again after any cleanup ambiguity,
the harness captures the exact synthetic engine inventory, recorded daemon
identity, database integrity/current operations, service logs, and a `/proc`
process tree seeded from the daemon, engine sessions, and persisted child
identities. Command arguments are allowlist-redacted: paths, values, and
unknown executable names are never written verbatim. An evidence capture is
read-only and never expands the set of processes eligible for signaling.

## G3 — Package-manager acceptance

G3 exercises two already-built local `.deb` artifacts through the real Termux
package database and the fixed `termux-stacksd` runit service. Unlike S0–G2,
this harness intentionally changes the device's installed packages. It does
not build packages, download dependencies, access an OCI runtime, create a
release, or qualify the other three package architectures.

### Destructive-scope prerequisites

Run G3 only on a disposable or deliberately cleared Termux installation. The
harness refuses to perform its first package operation unless all of these are
true:

- `termux-stacks` has no package-database record;
- `$PREFIX/bin/termux-stacks`, the `termux-stacksd` service directory, and
  `$PREFIX/var/run/termux-stacks` are absent; the durable-state directory is
  either absent or a real, mode-0700 empty directory;
- `libsqlite`, `proot-distro` exactly 5.6.0, `termux-services`, and `runit` are
  already in the configured state, and `dpkg --audit` is empty;
- `service-daemon` has been started by restarting the Termux shell after the
  first `termux-services` installation;
- both inputs are absolute, non-symlink `.deb` files for the device
  architecture, the old package version sorts before the new one, and both
  release ELFs pass the layout/dependency inspection. The old candidate must
  implement protocol 1/schema 2; the new candidate must implement protocol
  2/schema 3.

The old side is not an arbitrary rebuild. Its repository-owned trust roots are
the S5 package from source
`1e0c34d2a4498c9f5660662f0dc008aefe1921ab`, package SHA-256
`dd09f17ba225700ce1a18a8477efd67117a42963f4f4f7ee757151d663e4f9b8`,
and extracted release ELF SHA-256
`78620c23c17d1deb97d0ed7030e47dbf75a2a4732f8eb8bfb7fdbf6fe2b7fc37`.
Preflight requires both hashes exactly. The dynamic schema and protocol checks
then prove that the transferred artifact still has the expected v1/v2
behavior; a version label alone is not accepted as provenance.

The explicit acknowledgement is mandatory:

```bash
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/g3.sh \
  --old-deb "$HOME/packages/termux-stacks-old.deb" \
  --new-deb "$HOME/packages/termux-stacks-new.deb" \
  --accept-package-manager-changes \
  --output-root "$HOME/termux-stacks-evidence"
```

The harness snapshots both artifacts into its private runtime before the
preflight and records their SHA-256 values. Review those values against the
four-architecture build ledger before accepting a run. Inputs are never
modified. Normal lifecycle operations use scripted `apt-get`, because G3 must
exercise the dependency resolver used for local package installation rather
than bypass dependency resolution and the user-facing lifecycle with
`dpkg -i`. Before every effect, `apt-get --simulate` must show a plan containing
only `termux-stacks`. Installs use `--no-install-recommends --no-remove` and an
exact absolute local artifact; `--allow-downgrades` permits the deliberate old
package replay after a newer residual conffile record. Ordinary removals and
the final purge use `--no-auto-remove`. The installed version and ELF hash are
checked after every install or upgrade. Already configured dependencies
therefore remain under APT control without authorizing another package change.
The harness never updates indexes, installs a dependency, repairs unrelated
package state, or falls back to a raw `dpkg` mutation. Each real APT effect is
also bracketed by a sorted inventory of the complete dpkg database; removing
the exact `termux-stacks` row must leave those inventories byte-identical.
The absolute local artifact is the only install target. Any unrelated package
change fails the gate and stops further lifecycle effects. The harness does
not pass `--no-download`: Termux APT 2.8.1 aborts through Android fdsan on that
local-package path, while the simulated plan and full dpkg inventory delta
retain the intended fail-closed scope.

Ordinary `apt-get remove` intentionally exercises Debian conffile behavior: it
must leave `deinstall ok config-files` and the fixed disabled service skeleton,
while removing the executable and stopping any qualified daemon. Only after
both ordinary-removal and reinstall cases pass does the exit handler authorize
one simulated, exact-package `apt-get purge termux-stacks`. Any earlier failure
preserves the package record and durable state for review instead of purging.

### Package and lifecycle matrix

Before installation, each artifact is extracted without executing its
maintainer scripts. Static checks require the one public executable, the
Apache-2.0 license link, the disabled runit layout, the three declared runtime
dependencies, a stripped architecture-matched PIE, and exactly
`libc.so`, `libdl.so`, and `libsqlite3.so` as dynamic dependencies. Package
data must not own the durable or ephemeral runtime directories. The canonical
Apache-2.0 payload is the exact
`share/doc/termux-stacks/copyright -> ../../LICENSES/Apache-2.0.txt` symlink.
The historical old artifact must declare
`libsqlite, proot-distro (>= 5.6.0), termux-services` exactly; the new package
must replace that historical range with the release pin
`proot-distro (= 5.6.0)`.
The service files must be the exact conffile set. The new package must contain
a removal-only `prerm` that creates the fixed `down` marker, requests a bounded
stop, and requires a bounded exact terminal-status plus PID proof without
deleting the service. Its stop and status diagnostics must remain visible in
APT output. The purge-only `postrm` deletes only that fixed service directory;
neither hook may name durable or ephemeral runtime state. Install hooks are
rejected.

The acceptance sequence is:

1. fresh-install the old package and prove the service is disabled, no daemon
   ran, and `dpkg --verify` is clean;
2. create one random, fsynced marker in the otherwise empty durable-state
   directory;
3. explicitly start the old daemon, require a successful read-only protocol
   round trip and a valid schema-2 database, record its installation ID, then
   disable and stop it;
4. upgrade old to new while disabled and prove that no daemon starts, schema 2
   and the installation ID remain unchanged, and no maintainer script performs
   the migration;
5. explicitly start the new daemon, require transactional migration to schema
   3 with the same installation ID and marker, verify status, then stop it;
6. ordinarily remove the disabled package and prove
   `deinstall ok config-files`: the executable and socket are gone, the exact
   disabled service skeleton remains, and the byte-identical schema-3 database
   and marker survive;
7. after that preservation proof, remove only the exact harness-owned SQLite
   database allowlist while retaining the marker, then reinstall the old
   package disabled;
8. enable the old service again, require a fresh schema-2 database, and qualify
   its boot ID, PID, start time, executable inode, argv, socket, marker, and
   new installation ID;
9. upgrade to the new package while live, then require the same old daemon
   identity for five seconds, continued enablement, and schema 2. The new
   protocol-2 CLI must exit nonzero with empty stdout and exactly
   `termux-stacks status: unsupported protocol version 1; expected 2`;
10. run the documented `sv restart termux-stacksd` remediation, require a new
    PID executing the installed new ELF, migrate to schema 3 with the same
    installation ID, and verify that the new CLI status succeeds;
11. ordinarily remove the enabled live package and require that the qualified
    daemon, socket, and binary disappear, the package enters
    `deinstall ok config-files`, the exact disabled service skeleton remains,
    and the byte-identical migrated database and marker survive;
12. reinstall the new package from that conffile state without
    `--force-confmiss`, and prove it is disabled without changing the preserved
    schema-3 state.

After step 12 authorizes final cleanup, the exit handler simulates and performs
one exact disabled purge. It proves that the package record and fixed service
directory disappear while the database and marker remain byte-identical.
Only then may it delete its marker, SQLite's known `state.db` files, and the
known daemon lock/socket after the qualified daemon has drained. It uses
`rmdir` only for a state directory created by this run and for its exact empty
runtime directory. It records the exact package-service `runsv` and `svlogd`
identities before purge and waits for both to disappear without targeting the
global `runsvdir`. A pre-existing empty mode-0700 state directory is retained
and restored empty. An unknown file, directory, symlink, process identity,
package state, or service residue fails cleanup and is preserved for review;
cleanup never broadens a path or process target. Before step 12, the purge and
all harness-owned state deletion remain unauthorized.

### Evidence and limits

The evidence bundle includes artifact control metadata and contents, extracted
layout inventories, ELF reports, package-manager stdout/stderr, every durable
intent, service-status snapshots, schema/version/integrity/installation-ID
snapshots, byte-hashed before/after state inventories, the simulated and actual
final purge output, the matrix, cleanup results, and a final `SHA256SUMS`. A
reviewer must run
`sha256sum -c SHA256SUMS` and compare the package hashes with the immutable G3
artifact ledger.

This one-device run qualifies Debian package lifecycle and runit behavior for
aarch64 only. The clean, pinned `termux-packages` builds for
`aarch64`, `arm`, `i686`, and `x86_64`, their artifact hashes and sizes, and
the immutable release archive remain separate G3 evidence. It does not
qualify Pacman removal: libalpm continues the transaction after a failing
`pre_remove` scriptlet, so Pacman cannot provide the Debian hook's fail-closed
stop guarantee and remains a G4 blocker.
