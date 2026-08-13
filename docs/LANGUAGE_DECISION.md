# ADR-001 — Rust per Termux Stacks

**Stato:** accettata
**Data:** 2026-08-13
**Decisione:** Rust, un package/crate e un binario

## Contesto

Termux Stacks è un demone locale failure-oriented: possiede stato durevole,
coordina processi PRoot, segnali, socket, SQLite e recovery dopo kill. Deve
essere distribuibile come package Termux su `aarch64`, `arm`, `i686` e
`x86_64`, con dipendenze e dimensione contenute.

Il linguaggio non cambia i contratti pubblici del manifest o del protocollo.

## Opzioni considerate

| Criterio | Rust | Go | Python |
|---|---|---|---|
| artefatto | ELF nativo | ELF nativo | interprete + moduli |
| stato/invarianti | enum, ownership, Result | type system semplice | controlli runtime |
| memoria daemon | nessun GC | GC | runtime/GC |
| concorrenza | esplicita | molto ergonomica | valida ma più runtime |
| SQLite native | FFI da confinare | cgo/alternative | stdlib |
| build Termux | helper ufficiale | helper ufficiale | supportato |
| prototipo | medio | rapido | molto rapido |
| scelta | **sì** | seconda scelta | no per il core |

Go era l'alternativa migliore e avrebbe ridotto tempi di compilazione e
complessità iniziale. Rust è preferito perché ownership di handle, connessioni,
processi e transazioni e transizioni esaustive hanno valore concreto in un
supervisore che deve fallire in modo prevedibile.

Python non è escluso dall'intero device: `proot-distro` 5.6.0 è esso stesso
un package Python. È escluso come runtime aggiuntivo del core Termux Stacks.

## Decisione

La prima implementazione usa:

- un package Cargo `termux-stacks`, `publish = false`;
- un binario `termux-stacks` con modalità CLI e `daemon`;
- `Cargo.lock` versionato e build release con `--locked`;
- MSRV iniziale Rust 1.93, compilata nel checkpoint host e inferiore al
  toolchain Termux 1.97.1 verificato il 2026-08-14;
- safe Rust nel core;
- `unsafe`/FFI confinati, motivati e testati;
- modello sincrono e pochi thread espliciti.

Non si introduce Tokio o un altro runtime async finché una misura non mostra
che accept loop, child watcher e log drain non sono gestibili semplicemente.
Non si crea un workspace multi-crate prima di un confine dimostrato.

## Motivazione Termux

Il builder ufficiale fornisce
[`termux_setup_rust`](https://github.com/termux/termux-packages/blob/master/scripts/build/setup/termux_setup_rust.sh)
e ricette ufficiali come
[`ripgrep`](https://github.com/termux/termux-packages/blob/master/packages/ripgrep/build.sh)
mostrano il percorso Rust. Questo dimostra supporto del toolchain, non
compatibilità automatica delle crate scelte.

Il device installa l'ELF e dipendenze native dichiarate; non scarica Cargo,
rustup, crate o toolchain al post-install o al primo avvio.

La
[packaging policy](https://github.com/termux/termux-packages/blob/master/CONTRIBUTING.md#packaging-policy)
scoraggia software che dovrebbe essere installato con `cargo`. Termux Stacks
non verrà quindi pubblicizzato tramite `cargo install`: il suo valore è
l'integrazione indivisibile con prefix, runit, `proot-distro` e Android.
L'ammissione ufficiale resta comunque discrezionale.

## Conseguenze

Positive:

- un solo processo nativo piccolo e aggiornabile;
- ownership e stati impossibili più visibili al compilatore;
- nessun garbage collector nel demone;
- dipendenze riproducibili con lockfile;
- boundary Unix/SQLite isolabili.

Costi:

- compile time e curva di apprendimento maggiori di Go;
- binding SQLite e parser YAML devono essere qualificati su quattro arch;
- alcune API Bionic possono richiedere FFI;
- safe Rust non sostituisce intent persistiti, idempotenza e fault injection.

## Gate tecnici

Prima dell'MVP:

1. build package sulle quattro architetture;
2. scelta di un parser YAML mantenuto che rifiuti duplicati/tag/alias;
3. scelta SQLite con linking verificato e nessun download di dipendenze a
   runtime;
4. audit `cargo tree`, licenze, advisory e dimensione;
5. inventario di ogni `unsafe`;
6. misura RAM, startup e build su device;
7. fault test di processi e database.

Un fallimento di una crate non rimette automaticamente in discussione Rust:
si prova un adapter o una dipendenza più piccola. Il linguaggio viene
riesaminato solo se la toolchain o più primitive indispensabili falliscono
sistematicamente sui target Termux.

## Non decisioni

Questo ADR non sceglie:

- parser YAML;
- binding o PRAGMA SQLite;
- libreria CLI;
- framing IPC;
- libreria signal/socket;

Queste decisioni devono uscire dagli spike con misure, non essere congelate
prima del primo binario.
