# Termux Stacks Architecture

**Status:** v0.1 proposal; M1/G2 completed on aarch64, G3 package candidate open
**Target:** Termux/Android without root access
**Verified engine baseline:** `proot-distro 5.6.0`
**Authority:** internal components, persistence, and recovery

## 1. Design criterion

The architecture optimizes for reliability and comprehensibility on a single
phone, not for theoretical scalability. Every component must justify a real,
already observed failure mode.

Rules:

1. use one process and one source of truth before introducing coordination;
2. serialize before parallelizing;
3. delegate to the engine what the engine already knows how to do;
4. persist intent before external effects;
5. do not automate when observation is ambiguous;
6. provide no public abstraction for deferred features.

## 2. Frozen decisions

- A single public Rust executable: `termux-stacks`.
- A single initial package/crate; boundaries are Rust modules.
- A single global daemon, `termux-stacks daemon`, running in the foreground
  under runit.
- `termux-stacksd` is the runit service ID, not a binary artifact.
- A short-lived CLI and a local Unix socket.
- One global FIFO queue for all mutations.
- SQLite is the sole transactional source of truth.
- An advisory lock file held open makes the daemon a singleton; the socket is
  IPC only. There are no per-stack locks.
- `proot-distro` is accessible only through an adapter.
- A separate writable rootfs for each service.
- Cold recovery: stop/recreate only with sufficient evidence; otherwise use
  `unknown` with operational diagnostics.
- Synchronous implementation; an async runtime requires measurements and an
  ADR.

## 3. Context

```text
short-lived CLI
   │ local JSON, exact version
   ▼
termux-stacks daemon ───────────── runit
   ├── command queue (single writer)
   ├── manifest
   ├── SQLite: desired + operations
   ├── in-process supervisor
   └── adapter proot-distro
          ├── service A rootfs ── process/log
          └── service B rootfs ── process/log
```

Everything runs under the same Android UID. The diagram shows software
ownership, not kernel isolation.

## 4. Code structure

```text
src/
├── main.rs       # CLI/daemon dispatch
├── cli.rs        # arguments and output
├── manifest.rs   # parsing and validation
├── protocol.rs   # IPC types and framing
├── daemon.rs     # accept loop and mutation queue
├── store.rs      # SQLite and migrations
├── engine.rs     # trait + proot-distro adapter
├── supervisor.rs # child, log, exit, and restart
├── reconcile.rs  # startup and recovery
└── paths.rs      # layout under PREFIX
```

Do not create `domain`, `planner`, `control-plane`, `storage`, or `xtask`
crates until a boundary requires independent builds, dependencies, or
ownership.

## 5. CLI and protocol

`config validate` runs locally. All runtime reads and mutations go through the
daemon; the CLI does not open SQLite and does not start workloads.

The v0 protocol uses request/response JSON Lines over a Unix socket:

- one JSON object per line, at most 1 MiB;
- an exact `protocol_version` in every request;
- a unique `request_id` to deduplicate retries;
- one final result, with no streaming or cursors;
- version incompatibility = an error instructing the user to restart the
  service.

There are no negotiation ranges, subscriptions, backpressure, or remote APIs.
After an upgrade, the already running daemon may be older than the new binary:
the exact version prevents incompatible CLI and daemon versions from
continuing silently.

The M1 implementation uses protocol version 2. `up` carries the canonical
UTF-8 manifest base directory so the daemon can resolve and recheck relative
binds. `status`
returns one stack object containing a service array sorted by service name;
an absent stack returns exactly its name, `observed_state: "absent"`, and an
empty service array. `logs` and `restart` always identify one stack/service
pair. Log results contain separate stdout and stderr streams, use a default
and maximum tail of 200 lines per stream, and must fit in one bounded frame.

## 6. Daemon and concurrency

At startup, the daemon:

1. derives paths from the prefix;
2. prepares paths without following symlinks;
3. acquires the daemon's non-blocking advisory lock;
4. recovers any stale socket and binds the socket, without accepting requests;
5. opens SQLite, transactionally migrates an explicitly supported previous
   version, and otherwise requires the exact schema version;
6. performs the engine capability probe;
7. reconciles incomplete operations;
8. accepts requests.

The kernel releases the lock when the file descriptor is closed, including
after a crash. The file may remain on disk and contains no authoritative
state. Only the lock holder may replace a stale socket: binding the socket
alone is not a safe election algorithm.

The current mutation is the only logical writer. Reads and exit-status
collection may use a limited number of threads, but every state change returns
to the queue. SQLite is never left in a transaction during install, run, kill,
or lengthy I/O.

The daemon receives SIGTERM from runit, stops accepting mutations, and stops
each owned workload in reverse dependency order. This graceful control-plane
shutdown records proven exits but preserves each stack's desired state; it is
not implemented by calling the user-facing `down`. A later daemon may restart
only services whose stopped state was durably proven. A kill -9 is handled
only on restart and leaves any service that may still be active `unknown`.

## 7. Persistence

Minimal layout:

```text
$PREFIX/
├── var/lib/termux-stacks/
│   ├── state.db
│   ├── volumes/<stack>/<volume>/
│   └── logs/<stack>/<service>.{stdout,stderr}.log
├── var/run/termux-stacks/
│   ├── daemon.lock
│   └── daemon.sock
└── var/service/termux-stacksd/
```

The completed S5 checkpoint uses pre-release schema version 2 with four
tables:

- `meta`: schema and installation ID;
- `stacks`: desired state, stored manifest, and the S5 revision token;
- `services`: engine alias, rootfs state, process identity, state, and last exit;
- `operations`: request ID, intent, phase, and outcome.

The M1 implementation uses schema version 3:

- `meta`: schema and installation ID;
- `stacks`: desired/observed state, committed manifest and manifest base, and
  an integer committed revision;
- `services`: logical service state, serialized effective configuration,
  current-generation reference, and persisted restart window;
- `rootfs_generations`: non-reusable alias, owner, image, role
  (`candidate | current | retired`), rootfs state, process/session identity,
  separate exit code/signal, and log paths;
- `operations`: stack-wide request, candidate revision/configuration, phase,
  response, and outcome;
- `operation_services`: deterministic ordinal and durable phase/outcome for
  every service affected by an operation.

`operations` and `operation_services` are the journal. Environment,
dependencies, mounts, volumes, and ports do not require separate tables:
their validated effective configuration remains in the committed or candidate
manifest. Volume paths are deterministic, and global port conflicts can be
checked by scanning committed manifests while the single mutation queue is
held. There are no separate journal files, snapshots, event stores, or
compaction.

The version-2 to version-3 migration is one SQLite transaction. It preserves
the installation ID, stack/service configuration, exact alias, logs, and a
proven stopped/installed generation. A stack with a successful version-2 `up`
becomes committed revision 1; a stack without a successful `up` has revision
0 and no committed manifest. Because the new daemon cannot inherit an old
live child handle, any version-2 active, effect-invoked, or incomplete state
migrates to `unknown` without an engine call. `user_version` changes only in
the committing transaction. An unknown schema is never modified: the database
is preserved and startup fails with diagnostics.

Initial durability:

- SQLite transactions and foreign keys enabled;
- the S5 checkpoint uses the system SQLite library, `journal_mode=DELETE`,
  `synchronous=FULL`, a five-second busy timeout, and no transaction across an
  engine call; the Android acceptance run qualified transaction rollback and
  integrity after a real `SQLITE_FULL` result;
- intent committed before every effect;
- outcome committed after observing the effect;
- storage/full errors handled before proceeding.

S5 persists a separate `rootfs_state` and enough operation phases to avoid
guessing after a cold start:

- `intent` or `logs_prepared`: the engine could not have been invoked, so the
  service becomes `failed`, the rootfs is `absent`, and an explicit retry uses
  a new alias;
- `installed`: install completed before start was invoked, so the service
  becomes `failed`, the rootfs remains `installed`, and an explicit retry may
  reuse the same alias;
- `install_invoked`, `start_invoked`, `started`, `committed`, or `stopping`:
  an external effect may exist, so loss of the live child handle becomes
  `unknown` and authorizes no automatic start, stop, retry, or deletion.

The crash and storage hooks used to qualify these boundaries are compiled
only in debug builds. Release binaries contain neither the checkpoints nor
their environment-variable names.

M1 applies the same phase boundary independently to every
`operation_services` row. A service that has not reached an engine invocation
can be classified from durable ordering; `install_invoked`, `start_invoked`,
`started`, or `stopping` becomes `unknown` when the live handle is lost. The
stack is `unknown` if any required service is ambiguous, and reconciliation
does not continue the remaining DAG.

## 8. Engine contract

The adapter uses only public `proot-distro` commands and does not directly
read or modify its Python modules, databases, or rootfs internals.

v0 operations:

- capability probe;
- image/archive installation with an alias;
- foreground run;
- session list/ps;
- kill by the exact session identifier emitted by `proot-distro ps`, only
  after the S3 identity and ownership qualification;

`--detach` is prohibited: it discards stdio and removes the process from
direct supervision. The adapter must capture stdout, stderr, and exit status.

The v0 profile starts workloads with:

```text
proot-distro run --isolated [--env K=V] [--bind SRC:DEST] ALIAS [-- ARG...]
```

The adapter builds an argv vector, never a command string. If `command` is
absent, it omits `--`; if it is present, it passes at least one argument after
exactly one `--`. `--isolated` is the default, and only declared binds are
added back; shared home, shared tmp, and X11 are not implicit. `--minimal` is
outside v0 until it passes a dedicated spike. `login` and `--detach` do not
start production workloads.

### 8.1 Qualified session registry

S2 confirmed that the engine session registry is best effort. Denied
registration, denied reads, and a malformed JSON record can all produce empty
`ps --quiet` output with exit 0 while the independently observed process is
alive. The full table is human-readable output on stderr; it is not a stable
protocol. `--quiet` exposes only the registered PID, which is an engine
session identifier and does not by itself authorize a host signal.

While the daemon is alive, the child handle with `boot_id`, PID, and `/proc`
start time is the primary evidence; a positive registry row is complementary.
After a daemon crash, the handle is lost: an empty registry, an observation
error, or a malformed record means `unknown`, never `absent`. v0 does not
automatically start, recreate, or delete that service. Even a positive row
remains only complementary evidence and does not reconstruct ownership by
itself after a crash.

`run CONTAINER -- ARGS` preserves Entrypoint, replaces Cmd, and does not add a
shell. By contrast, `login -- COMMAND` wraps the command in the user's
configured shell with `-c`; it is not the runtime path. The adapter does not
invent a generic raw exec: it supports the verified semantics and rejects the
rest.

S3 qualified a single v0 strategy:

```text
proot-distro kill SESSION_PID
```

The target is the exact session identifier already observed and persisted
during the same daemon generation. It is never passed to `kill(2)`, never
replaced with the alias, and `--all` is never used.
The engine command propagates TERM to the tree, waits for its fixed grace
period, and escalates; through the inherited record holders, it also reaches
guests left alive after the PRoot tracer receives SIGKILL and descendants that
have changed PGID/SID. The spike also proved that sending TERM only to the
PGID is insufficient and that the exact target does not stop a second session
on the same alias.

The command's exit status alone does not prove that the workload stopped:
ownership preconditions and the available observation are required. If either
is lost, the outcome is `unknown`, and there is no fallback to host signals.
The engine grace period is best effort and not configurable; v0.1 does not
expose `stopGracePeriod`.

## 9. Identity and ownership

Each installation generates a random installation ID. Every attempt to create
a rootfs uses a non-reusable alias:

```text
txs-<installation-short>-<stack-short>-<service-short>-<random>
```

The intent containing the alias is committed before invoking the engine. A
prefix alone does not prove ownership, and an incomplete alias is not deleted
automatically.

A process requires at least the engine alias, observed PID, start time when
available, and boot identity. A saved PID alone does not authorize a signal.
The child handle is primary while it belongs to the current daemon; engine
`ps` is complementary, and an empty result never proves absence.

## 10. Service lifecycle

### Prepare

This procedure runs only if the rootfs is missing or the image changes. A
restart reuses the registered rootfs.

1. validate the manifest and capabilities;
2. generate a non-reusable alias;
3. insert a `PREPARE` operation;
4. invoke install;
5. observe success and register the rootfs.

A crash between steps 3 and 5 leaves an incomplete operation. Before the
durable `install_invoked` phase, recovery can prove `absent` and permits only
an explicit retry with a new alias. After install returns successfully, the
durable `installed` phase permits an explicit retry on the same proven rootfs.
While install may have begun but has not returned, the rootfs is `unknown`.
A negative engine inventory cannot strengthen that result. An `owned` partial
artifact is never started or reused and may be removed only by its exact full
alias; an ambiguous artifact is preserved for diagnosis.

For multiple services, the validated dependency graph yields one stable
topological order, using service-name order to break otherwise independent
choices. Preparation and startup use that order; stop uses its exact reverse.
Every service has its own non-reusable alias and operation phase.

### Start

1. record `START_NEW`;
2. open the separate stdout and stderr log files;
3. start `proot-distro run --isolated` in the foreground with separate argv
   entries;
4. capture stdout/stderr separately, exit status, and child/session evidence;
5. consider the service `running` when the main process is observed alive;
6. commit the revision when all services are running.

Application readiness is outside v0.1.

Normal process completion stores `last_exit_code`; signal termination stores
`last_exit_signal`. They are nullable and mutually exclusive. A successful
candidate is committed by promoting its generation references and integer
revision in one SQLite transaction.

### Stop

The adapter attempts the engine path verified by the spike and waits for its
escalation. If it cannot prove identity, it marks the service `unknown` and
does not send signals to potentially recycled host PIDs. The user receives a
diagnosis and a manual procedure.

### Restart and supervision

The synchronous daemon performs a bounded supervision tick between control
socket iterations. It reaps owned children, records a proven exit before any
new start, derives stack state, and starts only attempts whose durable backoff
deadline has elapsed. A long serialized engine operation may delay a tick, so
backoff values are minimum delays rather than scheduling deadlines.

The G2-qualified schedule uses exponential minimum delays of 1, 2, 4, 8, then
16 seconds, capped at 16 seconds, with one initial start and at most five
automatic retries in a 60-second window. The Android run measured both the
durable deadlines and the corresponding next starts. A process that remains
running for 60 seconds resets the window.
`no`, `on-failure`, and `always` never authorize a start when prior absence is
ambiguous. Every automatic or manual restart persists its own intent before
stop and start effects.

### Logs

Each generation writes stdout and stderr to separate private files. Log
retrieval reads backwards with both line and byte bounds; it does not load an
unbounded file or merge the two streams. The default and maximum tail is 200
lines per stream. Before writing a response, the daemon verifies that the
complete JSON frame fits the protocol limit and returns a bounded error if it
does not.

## 11. Reconciliation

At startup:

1. read the committed revision, desired state, and incomplete operations;
2. query the available child/session evidence for each service;
3. classify each session as `absent | active | ambiguous`;
4. classify every incomplete per-service phase in deterministic DAG order;
5. for `desired=stopped`, stop only targets with sufficient ownership;
6. for `desired=running`, restart only after proving prior absence;
7. derive the stack state, with any required ambiguous service making it
   `unknown`;
8. do not continue a partially completed DAG after ambiguity;
9. do not remove rootfs instances automatically.

The v0.1 strategy is stop-and-recreate when identity is proven, not adoption.
S2 demonstrated that the session registry can fail without an observable
signal: automatic restart after ungraceful loss of an active child handle
remains disabled. A proven normal exit or completed graceful shutdown is a
separate proof of absence and may permit policy-driven restart. A signal on
the foreground tracer alone does not prove guest-tree absence. Otherwise the
operator receives `unknown` and diagnostics; a possible duplicate is not
created.
S5 verified this behavior at every durable boundary and through 20 consecutive
crashes immediately after workload start. A recovery command that cannot
reconstruct the lost child handle changes neither the engine session nor the
rootfs.
G2 extended the same fail-closed contract to a partial service DAG, an
uncommitted multi-service parent operation, an interrupted reverse-order down,
and one proven-safe retry resumed from durable backoff.

## 12. Update

v0.1 deliberately separates stopping from replacement. `up` with a changed
manifest is accepted only after an explicit successful `down`; while the
committed graph is running it fails before any engine effect. The replacement
then creates an integer candidate revision and uses the exact existing
service-name set; adding or removing logical services is deferred. It uses:

```text
DOWN (explicit command) -> PREPARE -> START_NEW -> COMMIT
```

M1 preparation creates a fresh candidate generation for every service. The
candidate starts in stable topological order. Commit atomically promotes
candidates to `current`, marks replaced generations `retired`, stores the
candidate manifest/base, and advances the revision. No rootfs is deleted, and
named volume directories are independent of rootfs generations.

This is not an atomic runtime transition, and downtime is required. If
START_NEW fails while the same daemon still owns all required evidence, it
stops candidate services in reverse order. The last committed generations
remain retained but are not started automatically; retrying the last manifest
is an explicit operator action after diagnosis. Loss of any required identity
produces `unknown`; retired and ambiguous generations remain for manual
diagnosis. v0.1 has no public rollback command and no automatic GC.

## 13. Networking and mounts

The daemon checks for conflicts between manifests and makes a best-effort
check that a port is available before startup. It does not hold socket leases
or attribute ownership of the listener. A race between preflight and bind
remains possible.

Mounts are passed as public engine binds. The daemon canonicalizes host paths,
prohibits overlapping destinations, and does not promise read-only behavior.

## 14. Runit and Android

The recipe installs a single service:

```sh
exec "$PREFIX/bin/termux-stacks" daemon 2>&1
```

The `down` file leaves it disabled. The user runs
`sv-enable termux-stacksd`; post-install does not enable it.

Termux:Boot is optional and runs one-shot scripts. The recommended
configuration starts the `termux-services` infrastructure, not Termux Stacks
directly. After an Android force-stop, no component can restart until the user
reopens Termux. Reboots, Doze, and OEM kills remain best effort.

## 15. Security

The socket, database, logs, and volumes must be private to the Termux UID. The
daemon does not accept shared-storage paths for its state and does not listen
on TCP.

Workloads are trusted and can see resources belonging to the same UID. v0.1
does not support secrets as a feature: `--env VALUE` may appear in argv, and
file binds do not isolate data from the workload.

## 16. Deferred evolutions

These require separate evidence and ADRs:

- multiple writers or concurrent mutations;
- an async runtime;
- splitting into multiple crates or a separate runner;
- protocol negotiation and streaming;
- planner/lockfile/content addressing;
- jobs and migrations;
- configuration, secret, cache, and backup managers;
- auto-port, endpoint discovery, LAN, and declarative sockets;
- advanced probes;
- multi-stage updates, rollback, and GC;
- OCI builds, Compose imports, and interactive exec.

## 17. Official sources

- [PRoot-Distro v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/README.md)
- [Session registry v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/proot_distro/session.py)
- [Environment engine v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/proot_distro/commands/login/env.py)
- [Tree-kill v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/proot_distro/commands/kill.py)
- [termux-services](https://github.com/termux/termux-services)
- [Termux:Boot](https://github.com/termux/termux-boot)
