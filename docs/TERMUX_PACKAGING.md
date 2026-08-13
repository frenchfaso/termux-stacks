# Packaging Termux

**Stato:** percorso pre-release, non ricetta ufficiale
**Fonti verificate:** 2026-08-14

## 1. Esito

Termux Stacks è tecnicamente adatto a un package Termux non-root nel canale
`main`, ma un progetto nuovo non soddisfa ancora automaticamente i criteri
di ammissione. Il percorso realistico è:

1. build upstream e test su device;
2. release pubbliche e utenti reali;
3. eventualmente TUR;
4. package request;
5. recipe in `termux/termux-packages/packages/termux-stacks/`.

Solo la recipe dentro `termux/termux-packages` è canonica. Una
`packaging/termux/build.sh.fixture` upstream è un test/template e deve
dichiararlo chiaramente.

## 2. Blocker di candidatura

La policy ufficiale richiede, fra l'altro, progetto attivo/conosciuto,
licenza open source riconosciuta, package sotto 100 MiB e non duplicazione.
Indica inoltre che software normalmente installabile con package manager di
linguaggio, incluso `cargo`, dovrebbe usare quel canale.

Prima di una proposta servono quindi:

- licenza Apache-2.0 e file `LICENSE`;
- tag/release e source archive immutabili;
- community e manutenzione dimostrabili;
- differenza chiara da `proot-distro` e `docker-compose`;
- nessuna istruzione `cargo install` come canale utente;
- motivazione dell'integrazione Termux-specifica: prefix, runit,
  `proot-distro`, Bionic e lifecycle Android;
- feedback dei maintainer sull'uso di “Termux” nel nome;
- build e test sulle quattro architetture;
- dimensione package misurata.

Il rischio maggiore di ammissione è maturità/duplicazione percepita, non Rust.
Una recipe corretta non garantisce l'inclusione.

## 3. Layout upstream

```text
termux-stacks/
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── src/
├── tests/
├── docs/
└── packaging/termux/
    ├── README.md
    └── build.sh.fixture
```

Requisiti:

- un package Cargo, un binario, `publish = false`;
- `Cargo.lock` committato;
- `rust-version = 1.93`; il package Rust ufficiale era 1.97.1 alla verifica;
- build da source archive senza repository Git;
- nessun path FHS hardcoded;
- prefix configurabile in test;
- nessun download di dipendenze o self-update in post-install, avvio o
  runtime; `up` può acquisire esplicitamente un'immagine OCI;
- nessuna crate venduta come binario opaco.

Il builder può scaricare toolchain/crate secondo l'infrastruttura Termux.
`termux_setup_rust` stesso installa il toolchain nel cross-build: non si
deve quindi promettere una build completamente offline se non si introduce
vendoring deliberato.

## 4. Recipe sperimentale

Scheletro da completare dopo tag, licenza e spike delle dipendenze:

```bash
TERMUX_PKG_HOMEPAGE=https://example.invalid/termux-stacks
TERMUX_PKG_DESCRIPTION="Declarative service stacks for Termux using PRoot"
TERMUX_PKG_LICENSE="Apache-2.0"
TERMUX_PKG_MAINTAINER="<maintainer>"
TERMUX_PKG_VERSION="<version>"
TERMUX_PKG_SRCURL="https://example.invalid/termux-stacks/archive/refs/tags/v${TERMUX_PKG_VERSION}.tar.gz"
TERMUX_PKG_SHA256="<sha256>"
TERMUX_PKG_DEPENDS="proot-distro (>= 5.6.0), termux-services"
TERMUX_PKG_BUILD_IN_SRC=true
TERMUX_PKG_SERVICE_SCRIPT=(
  "termux-stacksd" 'exec "$PREFIX/bin/termux-stacks" daemon 2>&1'
)

termux_step_pre_configure() {
  termux_setup_rust
}
```

Note:

- homepage, checksum e maintainer sono placeholder invalidi;
- se SQLite usa la libreria di sistema, aggiungere `libsqlite` alle
  dipendenze e verificare l'ELF; non basta dichiararla;
- se una crate abilita SQLite bundled, la decisione richiede ADR e audit CVE;
- il build/install Cargo standard di `termux-packages` usa `--locked`,
  target e job configurati; la struttura single-package evita selezioni
  custom;
- non si installa un binario `termux-stacksd`.

L'helper ufficiale per i service script crea il file `down`, quindi il
servizio è installato disabilitato. Il comando utente è
`sv-enable termux-stacksd`.

## 5. Layout installato

```text
$PREFIX/
├── bin/termux-stacks
├── var/service/termux-stacksd/
│   ├── run
│   ├── down
│   └── log/run
├── var/lib/termux-stacks/       # creato a runtime
└── var/run/termux-stacks/       # effimero
    ├── daemon.lock
    └── daemon.sock
```

Regole:

- database, volumi e log non sono file posseduti dal package e sopravvivono
  agli upgrade/remove ordinari;
- lock file e socket vengono ricreati; il lock autorevole è quello kernel,
  non il contenuto del file;
- nessuno stato durevole vive in `$HOME` o shared storage;
- il runtime non legge internals in `$PREFIX/var/lib/proot-distro`;
- una purge esplicita futura deve mostrare target e richiedere consenso.

## 6. Servizio, installazione e upgrade

Runit esegue il daemon foreground. Stderr confluisce nello stream di logging
del servizio. Il post-install non abilita né avvia il servizio e non esegue
migrazioni lunghe.

Alla prima installazione di `termux-services`, l'utente deve riavviare la
shell perché parta `service-daemon`. Questo è un prerequisito operativo
documentato, non un comportamento controllato da Termux Stacks.

Dopo upgrade, il vecchio processo può continuare a eseguire l'ELF già
mappato. CLI e daemon includono una versione esatta di protocollo:

- compatibili: procedono;
- incompatibili: fail fast con `sv restart termux-stacksd`;
- nessun restart automatico dal post-install.

Prima del primo upgrade di formato supportato, il daemon crea un DB vuoto,
accetta solo la versione esatta e conserva senza modifiche uno schema
sconosciuto. Backup/migrazioni entrano con la prima transizione reale.

La rimozione è distinta dall'upgrade. Un `prerm` futuro deve, soltanto nella
vera rimozione:

1. eseguire best effort `sv-disable termux-stacksd`/`sv down`;
2. impedire retry runit verso un ELF rimosso;
3. non cancellare database, volumi, log o rootfs.

La semantica precisa Debian/pacman va implementata e testata nella fixture,
inclusi servizio enabled, disabled e upgrade, prima del gate package.

## 7. CI e device test

Upstream per ogni change:

- `cargo fmt --all -- --check`;
- `cargo clippy --all-targets --locked -- -D warnings`;
- `cargo test --locked`;
- test fake-engine e fixture manifest.

Dopo scelta delle crate native:

- build package `aarch64`, `arm`, `i686`, `x86_64`;
- verifica `Cargo.lock`;
- audit licenze/advisory;
- `readelf`/equivalente su NEEDED e dipendenze dichiarate;
- dimensione `.deb` e installata;
- build pulita con le modalità CI Termux previste;
- installazione del `.deb` in un Termux pulito, con dipendenze risolte dal
  package manager.

Su almeno un device aarch64:

1. installare il package;
2. verificare che `termux-stacksd/down` esista;
3. abilitare e controllare log/stato;
4. eseguire gli spike engine;
5. provare upgrade con daemon vivo;
6. provare kill -9, disk full controllato e recovery;
7. verificare remove con servizio enabled/disabled e che i dati sopravvivano.

Reboot, Doze, force-stop e più device diventano gate RC, non del primo
binario.

## 8. Termux:Boot

Termux:Boot è opzionale e non una dipendenza del package. L'add-on deve
provenire da una sorgente/firma compatibile con l'app Termux, essere aperto
almeno una volta e ricevere uno script eseguibile sotto `~/.termux/boot/`.
La configurazione raccomandata fa avviare allo script l'infrastruttura
`termux-services`; runit decide quali service senza file `down` avviare.

Termux:Boot è un launcher one-shot, non un watchdog. Dopo force-stop Android
l'utente deve riaprire Termux. Wake lock e boot sono scelte esplicite
dell'utente.

## 9. Checklist per proposta ufficiale

- [ ] nome approvato o rinominato prima dei path stabili;
- [x] licenza SPDX Apache-2.0;
- [ ] progetto attivo, release e utenti;
- [ ] source archive/tag/checksum immutabili;
- [ ] differenza da engine e Compose documentata;
- [ ] nessun `cargo install` promosso;
- [ ] build quattro architetture;
- [ ] package sotto 100 MiB con margine;
- [ ] servizio disabilitato per default;
- [ ] upgrade e rimozione conservano stato;
- [ ] maintainer disponibile;
- [ ] package request raccomandata prima della PR;
- [ ] recipe ufficiale mantenuta solo in `termux-packages`.

## 10. Fonti

- [Packaging policy](https://github.com/termux/termux-packages/blob/master/CONTRIBUTING.md#packaging-policy)
- [Creare un package](https://github.com/termux/termux-packages/wiki/Creating-new-package)
- [Build dei package](https://github.com/termux/termux-packages/wiki/Building-packages)
- [Helper Rust](https://github.com/termux/termux-packages/blob/master/scripts/build/setup/termux_setup_rust.sh)
- [Installazione service script](https://github.com/termux/termux-packages/blob/master/scripts/build/termux_step_install_service_scripts.sh)
- [termux-services](https://github.com/termux/termux-services)
- [Recipe proot-distro](https://github.com/termux/termux-packages/blob/master/packages/proot-distro/build.sh)
- [PRoot-Distro v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/README.md)
