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
[docs/evidence/S4.md](docs/evidence/S4.md). There is no usable runtime yet;
the S5 vertical slice is the next implementation milestone.

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

## First usable result

The first vertical slice supports one service and only these commands:

```sh
# after installing termux-services and restarting the shell
sv-enable termux-stacksd
termux-stacks config validate ./termux-stacks.yaml
termux-stacks up ./termux-stacks.yaml
termux-stacks status notes
termux-stacks down notes
```

This validates the complete manifest → daemon → SQLite → `proot-distro` →
log → recovery path. The subsequent MVP adds multiple stacks and services,
simple dependencies, environment variables, volumes, fixed loopback ports,
restart, and logs. Jobs, secret management, builds, automatic ports, advanced
update/rollback, and Compose compatibility are deferred.

Planned MVP manifest:

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

The spike baseline is `proot-distro 5.6.0`; the package will also require
`termux-services`. Termux:Boot remains optional, and startup after reboot is
best effort. No process can survive an Android force-stop of the Termux app.

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
