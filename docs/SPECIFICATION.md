# Termux Stacks Product Specification

**Status:** v0.1 proposal; S0–S5 completed, multi-service MVP open
**Target:** Termux on unrooted Android
**Authority:** public product behavior

## 1. Definition

Termux Stacks is a declarative local orchestrator. It brings multiple service
stacks to their desired state on a single Termux installation, using
`proot-distro` as the PRoot engine and runit to keep the control plane
running.

Termux Stacks adds what is missing between a single PRoot rootfs and a
minimal Compose-like experience:

- a multi-service manifest;
- `up`, `down`, `status`, `logs`, and `restart` lifecycle operations;
- simple startup dependencies;
- separate rootfs instances, explicit volumes, and per-service logs;
- restart with backoff;
- durable state and recovery after a control-plane crash;
- honest diagnostics about Android limitations.

It does not reimplement OCI artifact acquisition during `install`, rootfs
extraction, or tree-kill: it delegates them to the public
`proot-distro` interfaces.

## 2. v0.1 Scope

The initial vertical slice supports one stack with one service. It is a
technical gate and is not presented as an MVP.

The v0.1 MVP MUST then support:

1. multiple independent stacks on the same device;
2. multiple services per stack, with one replica per service;
3. an acyclic `dependsOn` graph that determines startup order;
4. OCI images installable by `proot-distro`;
5. `command` as an override of the OCI Cmd, without overriding the Entrypoint;
6. literal environment variables;
7. explicit bind mounts and named volumes;
8. fixed, declarative TCP ports on `127.0.0.1`, without NAT;
9. `no`, `on-failure`, and `always` restart policies, with bounded backoff;
10. file-based logs and an exit status for each service;
11. stop-and-recreate revisions that retain replaced rootfs generations;
12. a `running | stopped` desired state;
13. conservative recovery: restart only when absence or the target is proven;
    otherwise, report `unknown` with diagnostics for manual intervention;
14. runit service installation disabled by default;
15. explicit errors for unsupported fields or capabilities.

## 3. Non-goals

Termux Stacks v0.1 DOES NOT promise:

- security isolation between services;
- distinct UID, PID, mount, IPC, UTS, or network namespaces;
- cgroups or hard limits on CPU, RAM, I/O, and processes;
- a firewall, per-service DNS, virtual IPs, NAT, or port mapping;
- truly read-only mounts;
- full compatibility with Docker, Dockerfile, or Compose;
- multiple replicas per service;
- jobs, migration hooks, or exactly-once semantics;
- OCI builds;
- automatically assigned ports or typed service discovery;
- a secret manager, configuration manager, cache manager, or generic backup;
- separate readiness, liveness, and startup probes;
- atomic updates, zero downtime, or data rollback;
- operation after an Android force-stop;
- continuous availability under Doze, memory pressure, or OEM policies;
- cross-device orchestration.

These features require a new milestone and, when they change the contract,
an ADR. They must not be anticipated through public placeholders.

## 4. Minimal Model

```text
Stack
├── Revision
├── Service
│   └── RootfsGeneration
└── Volume

Operation records the intent and result of a mutation.
```

### Stack

The namespace and lifecycle unit. Its name is unique within the installation.

### Revision

A sequential version of the manifest accepted by the daemon. v0.1 does not
expose canonical digests, lockfiles, or portable revisions. A new
configuration creates a new revision; it is committed only after all required
services have started successfully.

### Service

Describes a long-running process. It owns a rootfs; the daemon does not start
a new session until it has ruled out a previous one. Independent processes
must be separate services.

### RootfsGeneration

A `proot-distro` container with a Termux Stacks alias. It is not immutable:
the guest may write to it. It is reused when restarting the same service and
replaced when the image changes. A generation is `candidate`, `current`, or
`retired`. Promotion changes which generation is current; it does not delete
the retired rootfs. Two services never share the same writable rootfs.

### Operation

Records at least the stack, type, phase, candidate revision, timestamp, and
result. A stack-wide operation with multiple services records a durable phase
and outcome for each service. The intent is committed before every external
effect. There is no second authoritative journal outside SQLite.

## 5. Invariants

The runtime MUST maintain:

1. at most one mutating daemon per installation;
2. at most one global mutation in progress;
3. the daemon does not intentionally start a second session for a
   stack/service and does not restart when absence has not been proven;
4. a writable rootfs may belong to at most one service;
5. no automatic removal of a rootfs with ambiguous ownership;
6. intent persisted before install, start, or stop;
7. no SQLite transaction held open while waiting for an engine command;
8. a PID is never used alone as proof of identity;
9. a fixed-port conflict is treated as an error;
10. ambiguous state is treated as `unknown`, never as success;
11. persistence is guaranteed only for declared volumes and bind mounts;
12. no API is described as secret-safe if it can expose the value.

For the `proot-distro 5.6.0` baseline, empty `ps --quiet` output with exit 0
is not proof of absence. In the current daemon, the exit of an owned child is
strong evidence; after the handle has been lost, an empty, unreadable, or
malformed registry results in `unknown` and does not authorize an automatic
effect.

## 6. Lifecycle

### up

`up` validates the manifest, persists one operation, prepares rootfs
instances, starts the configuration, and finally commits the revision. A
changed manifest for a running stack is rejected before any engine effect;
the operator MUST complete an explicit `down` first. The M1 update profile
keeps the exact committed service-name set; adding or removing a service from
an existing stack is deferred and fails before any engine effect.

```text
DOWN (explicit command) -> PREPARE -> START_NEW -> COMMIT
```

Services are prepared and started in a deterministic topological order. A
partial failure stops services started by the candidate in reverse order only
while their handles and identities remain owned by the same daemon. The last
committed generations remain retained but are never restored automatically.
If any required observation is ambiguous, the stack becomes `unknown` and no
further engine effect is attempted.

### down

`down` persists `desired=stopped`, terminates services in reverse order, and
retains rootfs instances, logs, and volumes. Permanent removal is not part of
v0.1.

```text
STOP_REQUESTED -> STOPPING -> STOPPED
```

After a crash, every incomplete phase retains `desired=stopped`: the daemon
resumes stopping only for targets with proven identity.
The v0 stop operation uses the exact engine session identifier; it does not
signal the host PID, broaden the target to the alias, or expose a configurable
grace period. If the identity precondition is lost, the service becomes
`unknown`.

### restart

`restart` restarts the process on the same rootfs. It does not create a
revision and is not an update mechanism.

```text
RESTART_REQUESTED -> STOPPING -> STARTING -> RUNNING
```

After a crash, the same conservative rules apply: no new start if the absence
of the previous session has not been proven.

Graceful daemon shutdown is distinct from `down`. SIGTERM stops accepting new
mutations, stops owned workloads with the qualified engine target, and records
their exit without changing `desired=running` to `stopped`. A subsequent
daemon may restart only services whose absence was durably proven. SIGKILL or
loss of identity still produces `unknown`.

## 7. Observed State and Restart

A stack is `stopped`, `starting`, `running`, `restarting`, `failed`, or
`unknown`.
A service is `absent`, `starting`, `running`, `stopping`,
`stopped`, `restarting`, `failed`, or `unknown`. A restarting service exposes
its durable `backoff` effect phase and next-attempt timestamp separately.

Stack state is derived deterministically: `unknown` if a service is
ambiguous; `stopped` if the desired state is stopped and no session is
active; `running` if all required services are running; `failed` if a
required service has exhausted its restart policy; `restarting` if at least
one service is in durable backoff; `starting` in all other convergence cases.

`absent` means that no rootfs is registered; `stopped` means that the rootfs
exists and the current daemon observed the exit of the process it owns. A
registry that is merely empty after a cold start is not sufficient to derive
`stopped`.

`running` means that the main process is observed, not that the application
is ready. A declared port may be checked for reachability, but v0.1 cannot
reliably attribute ownership of the listener to a specific PRoot process.

Normal exit is recorded as an integer exit code. Signal termination is
recorded separately as a signal number; the two values are mutually
exclusive. With the qualified engine, observing only a signalled foreground
tracer does not prove that the complete guest tree is absent, so v0.1 records
that case as `unknown` and does not restart it automatically. `on-failure`
therefore applies to a proven normal non-zero exit in v0.1; `always` applies to
proven normal exits. Restart policy never weakens the identity rules.

The initial backoff candidate is 1, 2, 4, 8, then 16 seconds, capped at
16 seconds, with one initial start and at most five automatic retries in a
60-second crash-loop window.
It is a required candidate for device qualification, not yet a measured
release guarantee. A process that remains running for 60 seconds resets the
window.

## 8. Networking and Storage

All services use Android's shared network. Local communication uses
`127.0.0.1:<fixed-port>`; the manifest must configure the application to
actually listen on that port. The `ports` declaration declares and verifies
on a best-effort basis; it neither reserves nor maps ports.

A named volume resides in Termux Stacks' private state directory and survives
restarts, a new image, and `down`. A bind mount mounts an explicit host path.
Android shared storage is not used implicitly.

## 9. v0.1 CLI

Vertical slice:

- `config validate FILE`;
- `up FILE`;
- `status STACK`;
- `down STACK`.

MVP:

- `logs STACK SERVICE [--tail N]`;
- `restart STACK SERVICE`.

`logs` addresses exactly one service and returns stdout and stderr as separate
streams; it does not synthesize an ordering between them. `--tail` is a strict
decimal integer from 1 through 200 and defaults to 200. Each stream contains
at most that many lines. Invalid UTF-8 is represented lossily. A result that
cannot fit in one bounded protocol response is rejected rather than truncated
silently. v0.1 does not provide follow/streaming.

```json
{
  "stack": "notes",
  "service": "api",
  "tail": 200,
  "stdout": ["first line", "last line"],
  "stderr": []
}
```

Service enablement remains `sv-enable termux-stacksd`; control-plane status
and logs remain available through the `sv`/runit tools. `status` displays
stacks and services, so there is no second `ps` command.

For an existing stack, `status STACK` returns one stack object with integer
`revision` and a `services` array sorted by service name. Every service entry
contains its name, observed and rootfs states, current alias, optional session
identity, separate nullable `last_exit_code` and `last_exit_signal`, and the
two log paths. The v0.1 shape is:

```json
{
  "name": "notes",
  "desired_state": "running",
  "observed_state": "running",
  "revision": 2,
  "services": [
    {
      "name": "api",
      "observed_state": "running",
      "rootfs_state": "installed",
      "alias": "txs-a1b2c3-notes-api-deadbeef1234",
      "session_id": 123,
      "last_exit_code": null,
      "last_exit_signal": null,
      "stdout_log": "/data/data/com.termux/files/usr/var/lib/termux-stacks/logs/notes/api.stdout.log",
      "stderr_log": "/data/data/com.termux/files/usr/var/lib/termux-stacks/logs/notes/api.stderr.log"
    }
  ]
}
```

For an unknown stack, the exact minimal shape is:

```json
{"name":"missing","observed_state":"absent","services":[]}
```

The absent shape has no desired state or revision because neither exists.

Mutations always pass through the daemon. `config validate` is the only
command guaranteed to work offline; it does not run capability probes, pull
artifacts, or check ports. The daemon may reject a syntactically valid
manifest after its preflight checks.

The following are not v0.1 contracts: `plan`, `lock`, `pull`, `exec`, `run`,
`update`, `rollback`, `events`, `backup`, `restore`, and `gc`.

## 10. Guarantees and Android Limitations

| Property | v0.1 guarantee |
|---|---|
| isolation between workloads | none |
| no intentional duplicate start | the daemon restarts only when prior absence is proven |
| daemon crash recovery | stop/recreate only with sufficient evidence; otherwise `unknown` |
| persistence | only declared volumes and bind mounts |
| private network | none |
| default exposure | loopback declarations only |
| reboot | best effort if runit/Termux:Boot are configured |
| force-stop | requires Termux to be reopened manually |
| update | stop/recreate with downtime |
| rollback | not included in v0.1 |

Workloads must be trusted: they share Termux's Android UID and can access
resources readable by that UID.

## 11. Success Criterion

v0.1 is ready only when multiple multi-service stacks complete
`up/status/logs/restart/down` cycles on Android aarch64, volumes survive,
no rootfs is shared, and a daemon kill/restart campaign creates no observable
duplicates. An engine limitation that prevents this property must reduce the
public guarantee or stop the milestone.
