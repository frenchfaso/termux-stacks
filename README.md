# Termux Stacks

Termux Stacks is a local multi-service application orchestrator for
Termux/Android. Its user experience is inspired by Docker Compose, while its
execution model uses the primitives that are actually available without root:
`proot-distro` for OCI images, root filesystems, and PRoot processes;
`termux-services`/runit for the control plane.

It is not a kernel container runtime. Services share the Android UID, kernel,
and network with Termux: there are no namespaces, cgroups, firewall, truly
read-only mounts, or isolation between hostile workloads.

## Status

The **S0 bootstrap is complete on aarch64**. Host CI, the Android package,
and the runit service are green; device harness v3 completed with 24 PASS,
0 FAIL, and 0 SKIP. The stateful cycle verified enable/start, singleton
enforcement, stale-socket recovery, restart after SIGKILL, and final disable.
The reproducible record is in [docs/evidence/S0.md](docs/evidence/S0.md).
The **S1 spike is also complete**: 31 PASS checks qualified OCI
`Entrypoint`/`Cmd` composition, argv, working directory, ordinary environment,
and exit status; its record is in
[docs/evidence/S1.md](docs/evidence/S1.md). The **S2 spike is complete**:
16 PASS checks reproduced three session-registry false negatives and froze a
fail-closed policy; its record is in
[docs/evidence/S2.md](docs/evidence/S2.md). **S3 is complete as well**: the
stop strategy uses only the exact session identifier and drained cooperative
trees, TERM-ignoring trees, a descendant in a new session, and guests left
behind after the PRoot tracer died, plus 100 consecutive cycles. Its record is
in [docs/evidence/S3.md](docs/evidence/S3.md). **S4 is complete**: 16 PASS
checks qualified completed installs and three crashes in each acquisition
window, freezing a fail-closed ownership policy. Its record is in
[docs/evidence/S4.md](docs/evidence/S4.md). **S5 is complete on aarch64**: the
single-service vertical slice now implements strict manifest validation, the
versioned local protocol, SQLite journaling, and the real
`up/status/down` lifecycle. Its acceptance run completed with 33 PASS,
0 FAIL, and 0 SKIP across normal lifecycle, exact request replay, protocol
mismatch, cooperative and TERM-ignoring process trees, real SQLite-full
rollback, all six crash checkpoints, and 20 consecutive post-start daemon
crashes. The current source package and stripped Android release ELF were
also rebuilt and inspected. The record is in
[docs/evidence/S5.md](docs/evidence/S5.md). **M1 and G2 are complete on
aarch64**: the multi-service MVP passed its final Android run with 14 PASS,
0 FAIL, and 0 SKIP across two concurrent two-service stacks, logs and isolated
service restarts, ports, an image update with persistent volume data, bounded
restart/backoff, and four crash-recovery boundaries. The reviewed record is in
[docs/evidence/G2.md](docs/evidence/G2.md). The G3 four-architecture package
candidate remains open.

Only the essential architectural decisions are frozen:

- one Rust package/crate and one public executable, `termux-stacks`;
- a small CLI and one global daemon started as `termux-stacks daemon`;
- `termux-stacksd` is the runit service name, not a second binary;
- one daemon advisory lock, one local Unix socket, and one mutation queue;
- SQLite as the sole source of truth, including operation intents and results;
- an isolated adapter that uses only the public `proot-distro` CLI;
- one distinct writable rootfs per service and explicit persistence only;
- conservative recovery: halt processing and require intervention when state
  is ambiguous.

In particular, empty `proot-distro ps` output does not prove that a workload
is absent. While the daemon retains the child handle, it uses the PID, start
time, and boot ID; once that handle is lost, unobservable state becomes
`unknown` and does not authorize automatic restart, recreation, or deletion.

Within the same daemon generation, when the handle, persisted identity, and a
positive registry record agree, v0 stops the workload only with
`proot-distro kill <session-pid>`. It does not signal that host PID directly,
use the alias, or use `--all`; the exit status is accepted only together with
the ownership preconditions. The engine's grace period remains fixed and
best effort, so the manifest does not expose `stopGracePeriod`.

## MVP command surface

The MVP supports multiple stacks and services through this small command
surface:

```sh
# after installing termux-services and restarting the shell
sv-enable termux-stacksd
termux-stacks config validate ./termux-stacks.yaml
termux-stacks up ./termux-stacks.yaml
termux-stacks status notes
termux-stacks logs notes api --tail 200
termux-stacks restart notes api
termux-stacks down notes
```

This covers the manifest → daemon → SQLite → `proot-distro` → log → recovery
path for multiple services, including simple dependencies, literal environment
variables, volumes, binds, fixed loopback ports, restart, and bounded logs.
Jobs, secret management, builds, automatic ports, advanced update/rollback,
and Compose compatibility are deferred.

MVP manifest:

```yaml
apiVersion: termux-stacks/v1alpha1
kind: Stack

metadata:
  name: notes

services:
  api:
    image: ghcr.io/example/notes-api:1.4.0
    command: ["--listen", "127.0.0.1:8080"]
    environment:
      DATA_DIR: /data
    mounts:
      - type: volume
        source: data
        target: /data
    ports:
      - address: 127.0.0.1
        port: 8080

  web:
    image: ghcr.io/example/notes-web:2.3.0
    dependsOn: [api]
    environment:
      API_URL: http://127.0.0.1:8080

volumes:
  data: {}
```

`command` replaces the OCI `Cmd` and preserves any `Entrypoint`, matching
`proot-distro run CONTAINER -- ARG...`. `ports` does not create NAT: it
declares a port that the application must actually open on the shared network.

## Documentation

- [Product specification](docs/SPECIFICATION.md)
- [Manifest specification](docs/MANIFEST_SPEC.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Implementation plan](docs/IMPLEMENTATION_PLAN.md)
- [Rust decision](docs/LANGUAGE_DECISION.md)
- [Termux packaging](docs/TERMUX_PACKAGING.md)

English is the repository language for maintained documentation, code
comments, diagnostics, and contribution material.

Each topic has one normative source: public behavior in the product
specification, schema in the manifest specification, internal details in the
architecture, and work order in the implementation plan.

## Dependencies and operational limits

The qualified engine baseline is exactly `proot-distro 5.6.0`. The S5 package
also requires the system `libsqlite` and `termux-services`; SQLite is linked
dynamically rather than bundled. Termux:Boot remains optional, and startup
after reboot is best effort. No process can survive an Android force-stop of
the Termux app.

State remains under `$PREFIX`, never in shared Android storage. The service is
installed disabled and must be enabled explicitly by the user.

## Name, affiliation, and license

The pre-release identifiers are:

- product, repository, and package: **Termux Stacks** / `termux-stacks`;
- CLI: `termux-stacks`;
- runit service: `termux-stacksd`;
- manifest: `termux-stacks.yaml`.

Termux Stacks is an independent community project and is not endorsed or
supported by the Termux maintainers. Feedback on using “Termux” in the name
must be requested before the first public release.

The project is distributed under the [Apache License 2.0](LICENSE). This
license covers Termux Stacks; `proot-distro`, Termux, and other dependencies
retain their respective licenses.
