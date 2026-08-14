# Termux Stacks Manifest Specification

**Status:** v0.1 proposal; parser not yet implemented
**Schema:** `termux-stacks/v1alpha1`
**Authority:** manifest syntax and semantics

## 1. Principles

The manifest describes what Termux/PRoot can actually enforce. It does not
copy Compose fields that have no reliable semantics on Android.

The parser MUST:

- accept exactly one YAML document;
- reject custom tags, merge keys, anchors, aliases, and duplicate keys;
- reject unknown fields;
- enforce limits on file size, nesting, collections, and scalars;
- produce errors that include the field path and, when available, line and
  column;
- never perform interpolation, includes, or command execution.

v0.1 has no variables, `x-*` extensions, public canonicalization, manifest
digest, or lockfile.

## 2. Vertical Slice Profile

The first slice accepts exactly one service and only the following fields:

```yaml
apiVersion: termux-stacks/v1alpha1
kind: Stack
metadata:
  name: hello
services:
  app:
    image: docker.io/library/alpine:3.22
    command: ["/bin/sh", "-c", "while true; do date; sleep 5; done"]
```

The semantics of `command` were verified by the S1 spike on
`proot-distro 5.6.0`. It overrides only the OCI `Cmd`: it does not replace the
`Entrypoint` and is not a raw exec independent of the image. The runtime must
fail closed if the capability probe does not confirm this contract.

Any MVP field that has not yet been implemented must produce `unsupported`,
not be ignored.

## 3. MVP Schema

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
    restart: on-failure

  web:
    image: ghcr.io/example/notes-web:2.3.0
    dependsOn: [api]
    environment:
      API_URL: http://127.0.0.1:8080
    restart: always

volumes:
  data: {}
```

Allowed top-level fields:

| Field | Type | Required |
|---|---|---|
| `apiVersion` | exact string | yes |
| `kind` | exact string `Stack` | yes |
| `metadata.name` | name | yes |
| `services` | non-empty map | yes |
| `volumes` | map | no |

## 4. Names

Stack, service, and volume names:

- match `^[a-z][a-z0-9-]{0,47}$`;
- are case-sensitive but allow only lowercase characters;
- cannot begin with `termux-stacks-`;
- are unique within their respective namespace.

The runtime does not use the name directly as a path or engine alias: it
applies escaping and an installation identifier.

## 5. Services

Each service accepts:

| Field | Type | Default |
|---|---|---|
| `image` | non-empty string | required |
| `command` | non-empty array of strings; first element non-empty | absent: OCI command |
| `environment` | string-to-string map | `{}` |
| `mounts` | array of mounts | `[]` |
| `ports` | array of ports | `[]` |
| `dependsOn` | array of service names | `[]` |
| `restart` | enum | `no` |

A service represents a foreground process. If the application daemonizes and
the main process exits, the service has terminated.

## 6. Image and Command

`image` identifies either an image from an OCI registry or a local OCI image
archive accepted by `proot-distro install`. A plain rootfs tar is rejected: it
does not contain the manifest required by `proot-distro run`. v0.1 provides no
pull policy, does not promise digest resolution, and does not modify the global
image cache to force a refresh.

The recommended path for reproducible tests is a local OCI archive or a
controlled immutable tag. Mutable tags are acceptable only with an explicit
acknowledgement that a subsequent installation may use the cache.

The public `proot-distro` CLI, verified on-device in S1, defines these
semantics:

- if `command` is absent, the adapter omits `--`: `run` executes Entrypoint +
  Cmd, Cmd alone, or Entrypoint alone; it fails if both are absent;
- if `command` is present, the adapter emits exactly one `--`, followed by each
  element as a separate argument: the array replaces Cmd but preserves
  Entrypoint;
- without an Entrypoint, the first non-empty element of `command` is the
  program;
- `command: []` is invalid: `run ALIAS --` is equivalent to no override and
  cannot represent “clear Cmd”;
- v0.1 provides neither clear-Cmd nor a generic Entrypoint override.

The adapter constructs an argv vector: it does not concatenate, interpret,
expand, or add a shell. Spaces, empty strings after the first element,
metacharacters, and arguments beginning with `-` remain literal. The image may
naturally choose a shell in its own Entrypoint/Cmd or shebang.

v0.1 does not expose `workingDirectory`; the adapter does not pass
`--work-dir`. The OCI `WorkingDir` therefore remains active, with the engine
falling back to `/`.

## 7. Environment

`environment` contains literal values only. Variable names match:
`^[A-Za-z_][A-Za-z0-9_]*$`.

v0.1 rejects keys that `proot-distro` filters, rewrites, or uses for its own
operation:

```text
ANDROID_ART_ROOT ANDROID_DATA ANDROID_I18N_ROOT ANDROID_ROOT
ANDROID_RUNTIME_ROOT ANDROID_TZDATA_ROOT BOOTCLASSPATH
DEX2OATBOOTCLASSPATH EXTERNAL_STORAGE HOME USER TERM COLORTERM PREFIX TMPDIR
MOZ_FAKE_NO_SANDBOX PULSE_SERVER
```

All keys beginning with `PROOT_` or `LD_` are also reserved, including keys
unknown to the adapter's current version.

The capability profile may extend this list for a future engine version, but
must never reduce it silently. This restriction preserves the same semantics
between ordinary Linux images and rootfs images recognized as Termux.

Secrets, file references, host interpolation, and `fromEndpoint` are not
supported. In particular, the engine's public CLI passes `--env K=V` in its
host argv; v0.1 therefore does not accept a secret feature that would promise
not to appear in argv.

Each non-reserved literal key-value pair is passed as a separate `--env K=V`,
without a shell. For these keys, the manifest value replaces the OCI `Env`
entry with the same name; other non-reserved OCI variables remain available.
S1 verified override and addition with two ordinary keys; it does not
generalize that result to keys managed by the engine.

## 8. Mounts and Volumes

Volume mount:

```yaml
mounts:
  - type: volume
    source: data
    target: /data
```

Bind mount:

```yaml
mounts:
  - type: bind
    source: ./config
    target: /app/config
```

Rules:

- `type` is `volume` or `bind`;
- `target` is absolute, normalized, and does not contain `..`;
- a volume must be declared at the top level;
- a relative bind source is resolved against the manifest directory;
- the source must exist; the destination is validated within the limits
  exposed by the engine;
- duplicate or overlapping destinations are rejected;
- there is no `readOnly` flag: PRoot does not guarantee it.

A declared volume is a private directory managed by Termux Stacks. The v0.1
manifest does not expose drivers, quotas, backup, or lifecycle policies.

## 9. Ports

```yaml
ports:
  - address: 127.0.0.1
    port: 8080
```

Only `127.0.0.1` is allowed, and `port` is an integer from 1024 to 65535. A
port may be declared by only one service in the manifest. The daemon performs
a best-effort preflight; a collision produces an error and is not reallocated.

This field does not configure the process, create NAT, or prove ownership of
the listener. The workload command or environment must use the same port.
Automatic ports, LAN access, declarative Unix sockets, and discovery are
deferred.

## 10. Dependencies

`dependsOn` is an array of services. The graph must be acyclic. Starting a
dependent service waits until the predecessor process is observed alive; this
does not imply application readiness. Stop uses the reverse order.

If the predecessor fails before a dependent service starts, that service is
not started and the stack becomes `failed`. After the dependent service has
started, an exit of the predecessor does not stop it automatically; the
predecessor's restart policy operates independently.

## 11. Restart

Allowed values:

- `no`: no automatic restart;
- `on-failure`: restart only after a non-zero exit or a signal;
- `always`: restart while the desired state is `running`.

Backoff, window, and limit initially use internal defaults measured on-device.
They are not yet configurable in the manifest.

## 12. Validation

`config validate` performs the following offline:

- restricted parsing and schema validation;
- names and references;
- `command` absent or a non-empty array with a non-empty first element;
- environment names that are valid and not reserved by the engine;
- paths and types that can be verified without side effects;
- dependency cycle detection;
- internal mount and port conflicts.

`up` adds the following daemon-side checks:

- engine capabilities;
- image presence and shape;
- command-matrix compatibility;
- host-path access;
- conflicts with other stacks and runtime state;
- available space and rootfs preparation.

Successful offline validation does not guarantee that `up` will succeed.

## 13. Evolution

`v1alpha1` may change incompatibly before the first release. The
implementation rejects unknown versions and future fields; it does not
silently preserve data it does not know how to apply.

Jobs, builds, config/secrets, caches, probes, automatic endpoints, lockfiles,
policies, replicas, update hooks/policies, and rollback will require explicit
schema extensions after the minimal lifecycle has been validated.
