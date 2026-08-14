# ADR-001 — Rust for Termux Stacks

**Status:** accepted
**Date:** 2026-08-13
**Decision:** Rust, one package/crate and one binary

## Context

Termux Stacks is a failure-oriented local daemon: it owns durable state,
coordinates PRoot processes, signals, sockets, SQLite, and recovery after
being killed. It must be distributable as a Termux package on `aarch64`,
`arm`, `i686`, and `x86_64`, with a constrained dependency set and footprint.

The language does not change the public contracts of the manifest or protocol.

## Options considered

| Criterion | Rust | Go | Python |
|---|---|---|---|
| artifact | native ELF | native ELF | interpreter + modules |
| state/invariants | enums, ownership, Result | simple type system | runtime checks |
| daemon memory | no GC | GC | runtime/GC |
| concurrency | explicit | highly ergonomic | viable, but with more runtime overhead |
| native SQLite | FFI to be contained | cgo/alternatives | standard library |
| Termux build | official helper | official helper | supported |
| prototyping | moderate | fast | very fast |
| choice | **yes** | second choice | no for the core |

Go was the best alternative and would have reduced compilation time and
initial complexity. Rust is preferred because ownership of handles,
connections, processes, and transactions, together with exhaustive state
transitions, has concrete value in a supervisor that must fail predictably.

Python is not excluded from the device as a whole: `proot-distro` 5.6.0 is
itself a Python package. It is excluded as an additional runtime for the
Termux Stacks core.

## Decision

The first implementation uses:

- one Cargo package, `termux-stacks`, with `publish = false`;
- one `termux-stacks` binary with CLI and `daemon` modes;
- a committed `Cargo.lock` and release builds with `--locked`;
- an initial MSRV of Rust 1.93, used for the host checkpoint build and lower
  than the Termux 1.97.1 toolchain verified on 2026-08-14;
- safe Rust in the core;
- contained, justified, and tested `unsafe`/FFI;
- a synchronous model with a small number of explicit threads.

Tokio or another async runtime will not be introduced until measurements show
that the accept loop, child watcher, and log drain cannot be managed simply.
A multi-crate workspace will not be created before a demonstrated boundary
exists.

## Termux rationale

The official builder provides
[`termux_setup_rust`](https://github.com/termux/termux-packages/blob/master/scripts/build/setup/termux_setup_rust.sh),
and official recipes such as
[`ripgrep`](https://github.com/termux/termux-packages/blob/master/packages/ripgrep/build.sh)
demonstrate the Rust path. This establishes toolchain support, not automatic
compatibility for the selected crates.

The device installs the ELF and its declared native dependencies; it does not
download Cargo, rustup, crates, or a toolchain during post-installation or on
first launch.

The
[packaging policy](https://github.com/termux/termux-packages/blob/master/CONTRIBUTING.md#packaging-policy)
discourages software that should be installed with `cargo`. Termux Stacks will
therefore not be promoted through `cargo install`: its value lies in its
inseparable integration with the prefix, runit, `proot-distro`, and Android.
Official acceptance nevertheless remains discretionary.

## Consequences

Positive:

- one small, updatable native process;
- ownership and impossible states are more visible to the compiler;
- no garbage collector in the daemon;
- reproducible dependencies through the lockfile;
- Unix/SQLite boundaries can be isolated.

Costs:

- longer compilation times and a steeper learning curve than Go;
- SQLite bindings and the YAML parser must be qualified on all four
  architectures;
- some Bionic APIs may require FFI;
- safe Rust does not replace persisted intents, idempotency, or fault
  injection.

## Technical gates

Before the MVP:

1. build the package on all four architectures;
2. select a maintained YAML parser that rejects duplicates, tags, and aliases;
3. select a SQLite solution with verified linking and no runtime dependency
   downloads;
4. audit `cargo tree`, licenses, advisories, and size;
5. inventory every use of `unsafe`;
6. measure RAM usage, startup, and build time on a device;
7. run fault tests for processes and the database.

The failure of a crate does not automatically reopen the Rust decision: an
adapter or a smaller dependency should be tried first. The language will be
reconsidered only if the toolchain or multiple essential primitives fail
systematically on the Termux targets.

## Non-decisions

This ADR does not select:

- a YAML parser;
- a SQLite binding or PRAGMAs;
- a CLI library;
- IPC framing;
- a signal/socket library.

These decisions must emerge from spikes with measurements, rather than being
frozen before the first binary.
