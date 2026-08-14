# Termux Stacks implementation plan

**Status:** operational v0.1
**Strategy:** spikes first, vertical slice, then MVP
**Implementation:** Rust, one package/crate, one binary

**Progress:** S0–S5 completed on 2026-08-14. The scaffold, minimal CLI, daemon
stub, private paths, singleton lock, host CI, aarch64 package, and runit
integration are green. The v3 device harness completed with 24 PASS, 0 FAIL,
and 0 SKIP; the stateful runit cycle is PASS. Evidence and limitations are
recorded in [evidence/S0.md](evidence/S0.md). S1 qualified
`Entrypoint`/`Cmd`, argv, ordinary non-reserved environment variables,
working directory, and exit status with 31 PASS, 0 FAIL, and 0 SKIP. S2
reproduced three false negatives in the session registry with 16 PASS, 0 FAIL,
and 0 SKIP, requiring `unknown` after the child handle is lost. S3 qualified
exact-session kill and 100 drain cycles with 15 PASS, 0 FAIL, and 0 SKIP. The
records are [evidence/S1.md](evidence/S1.md),
[evidence/S2.md](evidence/S2.md), and [evidence/S3.md](evidence/S3.md). S4
qualified interrupted-install ownership with 16 PASS, 0 FAIL, and 0 SKIP:
positive public observations can establish `owned`, while a negative public
inventory after invocation may have begun is always `ambiguous`. Its record is
[evidence/S4.md](evidence/S4.md). S5 completed the single-service vertical
slice with 33 PASS, 0 FAIL, and 0 SKIP, including every durable crash boundary,
20 post-start crash cycles, tree-stop behavior, protocol mismatch, SQLite-full
rollback, and the current aarch64 package. Its record is
[evidence/S5.md](evidence/S5.md). M1 and G2 then completed the multi-service
MVP with 14 PASS, 0 FAIL, and 0 SKIP on aarch64: two concurrent two-service
stacks, the complete public lifecycle, resources, update retention,
restart/backoff, and four crash-recovery boundaries. Its record is
[evidence/G2.md](evidence/G2.md). The package fixture intentionally remains a
pre-release template: the public tag, four-architecture build, and GitHub
tarball checksum belong to G3, which remains open.

## 1. Architecture verdict

The direction is approved for starting the spikes. Implementing the MVP
directly as though the PRoot primitives were already reliable is not approved.

Four risks can change the product:

1. the `proot-distro` session registry is best effort;
2. `command` is not equivalent to a raw Entrypoint override;
3. signal propagation and graceful stop depend on PRoot;
4. engine installation and Termux Stacks ownership are not atomic.

Work proceeds through stop/go gates. A guarantee that does not pass testing on
a device is removed or narrowed; it is not compensated for with a larger state
machine.

## 2. Frozen scope

The bootstrap includes:

- `config validate`, `up`, `status`, `down`;
- one manifest, one stack, one service;
- a synchronous daemon, one advisory lock, one Unix socket, and one mutation
  at a time;
- SQLite with intent/outcome;
- a `proot-distro` adapter;
- one rootfs, a foreground process, direct stdout/stderr logs, and conservative
  recovery.

M1 added:

- multiple stacks and services;
- a `dependsOn` DAG;
- non-sensitive environment variables;
- volumes and binds;
- fixed loopback ports;
- stop-and-recreate revisions with retained rootfs generations;
- restart and log retrieval.

Everything else is deferred. The normative feature matrix is in
[SPECIFICATION.md](SPECIFICATION.md) and is not duplicated here.

## 3. Decisions to close with evidence

| Decision | Initial default | Gate |
|---|---|---|
| concurrency model | synchronous, limited threads | change only with measurements |
| YAML parser | maintained crate, to be selected | hostile fixtures |
| SQLite | system `libsqlite` preferred | 4-architecture build + fault test |
| SQLite journal/PRAGMA | not frozen | real Termux filesystem |
| IPC framing | JSON Lines, 1 MiB, exact version | frame and timeout tests |
| command | OCI default or limited override | engine matrix |
| stop | `proot-distro kill` | signal/tree test |
| automatic recovery | fail-closed confirmed | S2: empty does not prove absence |
| alias cleanup | never automatic when uncertain | install crash test |
| license | Apache-2.0 | decided on 2026-08-14 |
| name | `termux-stacks` pre-release | maintainer feedback |

The parser, SQLite binding, and CLI library must not be selected on popularity
alone: maintenance, native dependencies, `unsafe`, size, and the Termux build
are part of the outcome.

## 4. Initial structure

```text
termux-stacks/
├── Cargo.toml
├── Cargo.lock
├── src/
│   ├── main.rs
│   ├── cli.rs
│   ├── manifest.rs
│   ├── protocol.rs
│   ├── daemon.rs
│   ├── store.rs
│   ├── engine.rs
│   ├── supervisor.rs
│   ├── reconcile.rs
│   └── paths.rs
├── tests/
│   ├── fixtures/
│   └── engine/
├── packaging/termux/
│   ├── README.md
│   └── build.sh.fixture
└── docs/
```

Do not create a multi-crate workspace or `xtask`. Extract a module only when it
has a compilable and testable boundary that genuinely reduces coupling or
`unsafe`.

## 5. Phase S0 — Rust and package bootstrap — completed

**Objective:** demonstrate that the smallest correct artifact runs in Termux.

Deliverables:

- a Cargo package with `publish = false`, an explicit edition, and
  `rust-version = 1.93`;
- a committed `Cargo.lock`;
- a CLI with `--version`, `--help`, and the internal `daemon` subcommand;
- paths resolved from `PREFIX`, never hardcoded to `/usr` or `/run`;
- a Termux fixture exercised by an aarch64 package build, with source revision,
  checksum, and license recorded, that builds and installs a single ELF; the
  public release and checksum of its tarball are deferred to G3;
- a foreground `termux-stacksd` service script with stderr redirected to
  stdout;
- a `down` file and no automatic enablement.

Tests:

- `cargo fmt --check`, Clippy `-D warnings`, and unit tests;
- host build from a source tree without Git metadata;
- an aarch64 cross-build or package build;
- install, `--version`, and service start/stop on a device;

Exit criteria:

- the package and service work on an aarch64 device;
- no maintainer script or binary downloads crates, Cargo, rustup, or a toolchain
  during installation, startup, or runtime; the package manager may resolve
  declared dependencies, and `up` may explicitly acquire images;
- ELF size, package size, and Cargo/runtime dependencies are recorded.

## 6. Phase S1 — Command contract — completed

**Objective:** document what the engine actually executes.

Create four OCI fixtures:

1. Entrypoint + Cmd;
2. Cmd only;
3. Entrypoint only;
4. neither Entrypoint nor Cmd.

For each, test `run` without arguments and with arguments after `--`. Record
guest argv, exit status, working directory, and environment. Test
`login -- COMMAND` separately to demonstrate the user-shell boundary with
`-c`. PID/process tree and signal target belong to S2/S3.

Exit criteria:

- golden table verified on a device (31 PASS, 0 FAIL, 0 SKIP);
- manifest `command` has honest semantics and rejects unrepresentable cases;
- the runtime builds no shell concatenation.

## 7. Phase S2 — Session registry and observation — completed

**Objective:** determine when `proot-distro ps` is sufficient evidence.

The minimum suite run against `proot-distro 5.6.0` includes:

- T1: one visible session, normal exit, and pruning;
- T2: two concurrent sessions on the same alias and independent removal;
- F1: publication denied, with a live workload but empty `ps --quiet`;
- F2: read denied, with transient disappearance and subsequent reappearance;
- F3: truncated JSON on the same still-locked inode, omitted while the
  workload remains alive and pruned after exit.

Killing the root PRoot while the guest remains alive moves to S3, which also
qualifies the signal target. Loss of the real daemon and reconciliation move
to the S5 vertical slice. Synthetic PID reuse and `flock` faults do not add a
useful guarantee after the false negatives already observed and are not S2
gates.

The harness must observe the host process tree independently of `pd ps` to
identify false negatives. Each case preserves raw output, exit status, and an
independent observation in a corpus; a golden representation annotates the
expected meaning without introducing production parsing code yet.

Verified exit: **16 PASS, 0 FAIL, 0 SKIP** in the synthetic runtime, with a raw
and golden corpus. The decision is final for v0: an empty result is never
strong evidence on its own. During the lifetime of the same daemon, the child
handle and `boot_id/PID/starttime` are the primary evidence; the registry is
complementary. After the handle is lost, empty, error, or a malformed record
produces `unknown`: no automatic restart, recreate, or delete.

## 8. Phase S3 — Signal and tree-kill — completed

**Objective:** qualify process stop and ownership.

The executed suite uses:

- a cooperative process that handles TERM;
- a tree that ignores TERM;
- a child and grandchild;
- a process that changes session/process group;
- a PRoot tracer terminated with SIGKILL while the guests and record holders
  remain active.

S3 does not require the Termux Stacks daemon. Losing an artificial parent adds
no engine contract beyond the qualified cases; SIGTERM and SIGKILL of the real
daemon, reaping, and recovery are tested in the S5 vertical slice.

The matrix compared:

- TERM to the host PGID as a negative control;
- `proot-distro kill <session-pid>`, where the PID is returned by
  `proot-distro ps`;
- engine escalation;
- orphans after stop.

Aliases and `--all` are not production targets and are not exercised. The
cooperative workload completed 100 cycles; every drain jointly requires known
roles to be absent, no record holders, and empty qualified PGIDs/SIDs.

Verified exit:

- exactly one v0 stop strategy: exact session ID;
- a fixed timeout documented as best effort;
- no public `stopGracePeriod`;
- no observable guest in 100 cycles; C3 leaves the second session with the
  same alias intact.

## 9. Phase S4 — Ownership and crash during install — completed

**Objective:** classify what an interrupted `proot-distro install` leaves
behind before designing the SQLite/engine boundary.

The test uses only the engine's public CLI and disposable, random aliases that
are never reused. Its minimal matrix is one completed local OCI install, one
identity-qualified crash behind a loopback download barrier, and one
identity-qualified crash after the public layer-application phase marker.
Each interrupted window is repeated a small, bounded number of times. The
resulting alias is classified as:

- `absent`: durable phase ordering proves that the engine invocation could not
  have begun;
- `owned`: the disposable alias is observable and attributable to that attempt;
- `ambiguous`: the public interfaces are insufficient to prove either case.

`proot-distro 5.6.0` publishes the alias before download or extraction, and
its public container inventory can turn an enumeration error into an empty
result. S4 therefore never infers `absent` from a negative inventory after an
invocation may have begun. Such a result is `ambiguous`; durable phase proof
for `absent` is introduced with the persisted operation in S5.

S4 does not introduce SQLite, a daemon, workload startup, or revision commit.
The test neither uses nor modifies pre-existing aliases. Persisted intent and
transactional fault points are applied in the S5 vertical slice using this
outcome table.

Verified exit:

- raw + golden evidence for a completed install and two bounded interrupted
  windows, with each fault repeated three times;
- all seven attempts classified `owned` by two exact positive public
  observations, then removed by exact alias without residue;
- deterministic recovery rules for `absent`, `owned`, and `ambiguous`;
- `ambiguous` defined as fail-closed: no automatic deletion or startup;
- no test requires access to `proot-distro` internals;
- a manual procedure for questionable artifacts.

The aarch64 acceptance run completed with **16 PASS, 0 FAIL, and 0 SKIP**.
It left no test alias, process scope, synthetic rootfs, or change to the real
engine runtime. The complete matrix, artifact hashes, classifier, limitations,
and raw-bundle checksum are recorded in [evidence/S4.md](evidence/S4.md).

## 10. Phase S5 — Vertical slice — completed

**Objective:** a useful end-to-end path that is not yet multi-service.

**Status:** completed on 2026-08-14. The vertical slice implements the strict
parser, exact-version JSON Lines protocol, four-table SQLite journal, real
engine adapter, foreground supervision, separate logs, conservative cold
reconciliation, and `config validate/up/status/down`. The aarch64 acceptance
run completed with 33 PASS, 0 FAIL, and 0 SKIP. Host CI, the current native
Termux package, and the stripped release ELF are green. The reproducible
record is [evidence/S5.md](evidence/S5.md).

Deliverables:

- a strict parser for the vertical-slice profile;
- a production parser for engine output derived from the S2 raw + golden
  corpus;
- a singleton daemon through an advisory lock and local socket;
- an exact-version request/response protocol;
- SQLite `meta/stacks/services/operations`;
- `validate/up/status/down`;
- rootfs install, foreground run, log, and exit;
- reconciliation defined by the S2–S4 outcomes;
- a fake engine for host tests and a real adapter on a device;
- repetition under the real daemon of the signal/tree-kill tests selected in
  S3;
- SIGTERM and SIGKILL of the daemon while the engine child remains active, with
  fail-closed reaping/recovery;
- binary upgrade while the previous daemon is alive: diagnose a protocol
  mismatch without proceeding.

Required fault points:

1. before intent;
2. after intent, before the engine;
3. after install;
4. after start;
5. before commit;
6. during down.

Exit criteria:

- repeatable complete lifecycle, stopped-rootfs reuse, request replay, and
  active-daemon restart on aarch64;
- 20 kill/restart cycles transition safely to `unknown`, without duplicates
  or automatic engine effects;
- the database remains consistent after SIGKILL and an actual `SQLITE_FULL`
  failure on the daemon connection;
- cooperative and TERM-ignoring three-process trees drain through the exact
  engine session target;
- logs do not block the child, protocol mismatch fails closed, and
  unimplemented MVP fields fail as `unsupported`;
- the exact source commit builds into an inspected aarch64 Termux package with
  the required dynamic libraries and no debug fault hooks in the release ELF.

## 11. Phase M1 — Multi-service MVP — completed

**Status:** completed on 2026-08-14. Protocol version 2, schema version 3, the
eight vertical slices below, host CI, and the final aarch64 G2 device suite are
green. The accepted run completed with 14 PASS, 0 FAIL, and 0 SKIP; see
[evidence/G2.md](evidence/G2.md).

Incremental vertical slices, each complete with tests and recovery:

1. multiple stacks and namespaces;
2. multiple services, a deterministic `dependsOn` DAG, per-service operation
   phases, rootfs generations, schema 2-to-3 migration, and multi-service
   status;
3. update/revision: require an explicit completed `down`, prepare candidate
   generations, start the candidate, commit only after complete success,
   retain retired generations, preserve the exact logical service-name set,
   and qualify conservative partial-failure recovery;
4. literal environment variables;
5. named volumes and binds, including canonical manifest-base handling;
6. fixed loopback ports;
7. restart/backoff and graceful daemon shutdown that preserves desired state;
8. bounded two-stream `logs --tail` and per-service `restart`.

The M1 persistence cut introduces integer committed/candidate revisions,
logical services, separately owned rootfs generations, and per-service
journal rows. Migration from the S5 version-2 database is transactional and
never invokes the engine: an active or incomplete state without an inherited
child handle becomes `unknown`. Unknown schema versions remain untouched.

Startup and stop ordering are stable across runs. Independent nodes use
service-name order; stop is the exact reverse. If a partial operation loses
any required identity, the stack becomes `unknown` and the remaining DAG is
not continued. The retained committed revision is not restarted automatically
after a failed `up`; recovery is an explicit operator action and is not a
public rollback feature.

The G2-qualified restart timing is 1, 2, 4, 8, then 16 seconds, with a
16-second cap, one initial start, and at most five automatic retries in a
60-second window. The Android run measured every durable deadline and the
corresponding next start at or beyond those minimum delays.
Restart policy never authorizes a new process unless prior absence is proven.

`logs` addresses one service, returns stdout and stderr separately, defaults
to 200 lines and accepts only `--tail 1..=200`. Each stream and the complete
response are byte-bounded; an oversized response fails explicitly.

Do not develop two slices in parallel when they share an unresolved failure
mode. Every new SQLite column and state must correspond to a concrete user or
recovery question.

## 12. Proportionate CI

Checks proportionate to each phase:

| Trigger | Checks |
|---|---|
| every change, S0–S4 | fmt, Clippy, unit tests, and syntax checks for spike harnesses and fixtures |
| every change, from S5 | previous checks, parser fixtures, and fake-engine contract |
| main/dependencies | host integration tests and source-archive build |
| nightly or native crate | package build on all 4 Termux architectures |
| manual | smoke and fault tests on an aarch64 device |

Before the RC, add more Android versions, at least one second device,
reboot/Doze/storage pressure, and soak testing. Three devices and 72 hours do
not block the bootstrap.

Every device failure preserves versions, redacted argv, DB integrity result,
current operation, process tree, and logs. Tests must not export secrets.

## 13. Gates

### G0 — Feasibility

S0–S4 completed; command, sessions, signals, and ownership have a verified
contract. A failure narrows the scope or stops the project before the daemon.

### G1 — Vertical slice

Completed on 2026-08-14. S5 passed on aarch64 with recovery consistent with
the public guarantees; see [evidence/S5.md](evidence/S5.md).

### G2 — MVP

Completed on 2026-08-14. M1 and two simultaneous multi-service stacks passed
`up/status/logs/restart/down`, deterministic DAG start/reverse-stop, volumes,
fixed ports, the qualified restart/backoff schedule, and four crash-recovery
cases. An explicit `down` followed by `up` with a new image preserved declared
volume data and retained both replaced rootfs generations. The final aarch64
run completed with 14 PASS, 0 FAIL, and 0 SKIP; see
[evidence/G2.md](evidence/G2.md). G3 remains open.

### G3 — Package candidate

Four-architecture build, license, release archive/checksum, disabled service,
tested upgrade, and name feedback obtained.

### G4 — Official proposal

The project is active, has users/releases, and meets the current policy. Being
technically packageable does not guarantee acceptance.

## 14. Residual risks

| Risk | Response |
|---|---|
| unregistered session | fail closed, no auto-start |
| reused PID | multiple evidence sources, never PID-only |
| crash between SQLite and engine | intent-first, unique alias, classify |
| partial rootfs | preserve and report, manual cleanup |
| mutable OCI tag/cache | no promise of reproducible updates |
| Android kill/force-stop | runit/Boot best effort, public limitation |
| disk full/corruption | fault tests, stop mutations |
| engine output changes | adapter + fixture, fail closed |
| native crate does not cross-build | spike and fallback before the MVP |
| scope creep | deferred features require a gate/ADR |

## 15. Definition of Done v0.1

v0.1 is complete when a user can:

1. install the package without a Rust toolchain on the device;
2. explicitly enable the service;
3. validate and start two multi-service stacks;
4. inspect status and logs, restart, and stop;
5. preserve data in a volume across restart and a new image;
6. receive an error for an occupied port or unsupported feature;
7. kill and restart the daemon without silent duplication;
8. understand when manual intervention is required;
9. understand that PRoot is not isolation and Android remains best effort.

Python may be present as a transitive dependency of `proot-distro`. The
guarantee is that Termux Stacks does not install a Rust runtime or a second
package manager during installation/startup.
