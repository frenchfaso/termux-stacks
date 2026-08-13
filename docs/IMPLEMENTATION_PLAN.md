# Piano di implementazione Termux Stacks

**Stato:** operativo v0.1
**Strategia:** spike prima, vertical slice, poi MVP
**Implementazione:** Rust, un package/crate, un binario

**Avanzamento:** checkpoint host S0 verde: scaffold, CLI minima, daemon stub,
path privati, lock singleton, build release, Clippy, 9 unit test e 5 test
black-box del vero ELF. È presente un harness device S0 non distruttivo. La
fixture è ancora un template con checksum placeholder; package build e prova
aarch64 restano aperti.

## 1. Verdetto architetturale

La direzione è approvata per iniziare gli spike. Non è approvato implementare
direttamente l'MVP come se le primitive PRoot fossero già affidabili.

Quattro rischi possono cambiare il prodotto:

1. il session registry di `proot-distro` è best effort;
2. `command` non equivale a un override raw dell'Entrypoint;
3. signal propagation e graceful stop dipendono da PRoot;
4. installazione engine e ownership Termux Stacks non sono atomiche.

Si procede con gate stop/go. Una garanzia che non supera il test sul device
viene rimossa o ridotta; non viene compensata con una state machine più grande.

## 2. Scope congelato

Il bootstrap include:

- `config validate`, `up`, `status`, `down`;
- un manifest, uno stack, un servizio;
- un demone sincrono, un lock advisory, un socket Unix e una mutazione alla
  volta;
- SQLite con intent/outcome;
- un adapter `proot-distro`;
- un rootfs, processo foreground, log file e recovery conservativa.

L'MVP successivo aggiunge:

- più stack e più servizi;
- DAG `dependsOn`;
- environment non sensibile;
- volumi e bind;
- porte loopback fisse;
- restart e consultazione log.

Tutto il resto è differito. La feature matrix normativa è in
[SPECIFICATION.md](SPECIFICATION.md), non viene duplicata qui.

## 3. Decisioni da chiudere con evidenza

| Decisione | Default iniziale | Gate |
|---|---|---|
| modello concorrenza | sincrono, thread limitati | cambiare solo con misura |
| parser YAML | crate mantenuta da scegliere | fixture ostili |
| SQLite | system `libsqlite` preferito | build 4 arch + fault test |
| SQLite journal/PRAGMA | non congelati | filesystem Termux reale |
| framing IPC | JSON Lines, 1 MiB, versione esatta | frame e timeout test |
| command | default OCI o override limitato | matrice engine |
| stop | `proot-distro kill` | signal/tree test |
| recovery automatica | disabilitata se ambigua | session registry test |
| alias cleanup | mai automatico se incerto | crash install test |
| licenza | Apache-2.0 | decisa il 2026-08-14 |
| nome | `termux-stacks` pre-release | feedback maintainer |

Parser, binding SQLite e libreria CLI non devono essere scelti per popolarità
soltanto: manutenzione, dipendenze native, `unsafe`, dimensione e build
Termux fanno parte dell'esito.

## 4. Struttura iniziale

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

Non si crea un workspace multi-crate né `xtask`. Un modulo viene estratto
solo quando possiede un confine compilabile e testabile che riduce realmente
accoppiamento o `unsafe`.

## 5. Fase S0 — Bootstrap Rust e package

**Obiettivo:** dimostrare che il più piccolo artefatto corretto vive in
Termux.

Deliverable:

- package Cargo `publish = false`, edition esplicita e `rust-version = 1.93`;
- `Cargo.lock` versionato;
- CLI con `--version`, `--help` e subcommand interno `daemon`;
- path risolti da `PREFIX`, mai hardcoded su `/usr` o `/run`;
- fixture Termux materializzata con release, checksum e licenza, che
  costruisce e installa un solo ELF;
- service script `termux-stacksd` foreground con stderr su stdout;
- file `down` e nessuna abilitazione automatica.

Test:

- `cargo fmt --check`, Clippy `-D warnings`, unit test;
- build host da source tree senza metadata Git;
- cross-build o package build aarch64;
- install, `--version`, start/stop servizio su device;

Exit:

- package e servizio funzionano su un device aarch64;
- nessun download di dipendenze/toolchain Rust durante installazione,
  avvio o runtime; l'acquisizione esplicita di immagini tramite `up` è
  traffico applicativo previsto;
- dimensione ELF, package e dependency tree sono registrate.

## 6. Fase S1 — Contratto command

**Obiettivo:** documentare ciò che l'engine esegue davvero.

Creare quattro OCI fixture:

1. Entrypoint + Cmd;
2. solo Cmd;
3. solo Entrypoint;
4. né Entrypoint né Cmd.

Per ciascuna provare `run` senza argomenti e con argomenti dopo `--`.
Registrare argv guest, PID/process tree, exit status, working directory,
environment e signal target. Provare separatamente `login -- COMMAND` per
dimostrare l'effetto della login shell.

Exit:

- tabella golden verificata su device;
- `command` del manifest ha una semantica onesta e rifiuta i casi non
  rappresentabili;
- nessuna concatenazione shell è costruita dal runtime.

## 7. Fase S2 — Session registry e osservazione

**Obiettivo:** sapere quando `proot-distro ps` è evidenza sufficiente.

Test:

- una e più sessioni sullo stesso alias;
- crash normale, SIGKILL e PID reuse simulato;
- registrazione con directory non scrivibile;
- `flock` non disponibile o fallito, se riproducibile;
- registro troncato e output inatteso;
- parent harness che muore mentre la sessione resta viva.

L'harness deve osservare process tree host indipendentemente da `pd ps` per
individuare falsi negativi. Ogni caso conserva output raw, exit status e
osservazione indipendente in un corpus; una rappresentazione golden annota il
significato atteso senza introdurre ancora codice di parsing production.

Exit:

- corpus raw + golden sufficiente a implementare e regredire il parser in S5;
- condizione precisa in cui un risultato empty può essere considerato forte;
- decisione: auto-recovery abilitata, oppure stato `unknown` con diagnostica
  e intervento manuale.

Se un falso negativo non è rilevabile, **non** si promette restart automatico
post-crash nell'MVP.

## 8. Fase S3 — Signal e tree-kill

**Obiettivo:** qualificare stop e ownership del processo.

Workload:

- processo cooperativo che gestisce TERM;
- processo che ignora TERM;
- figlio e nipote;
- processo che cambia sessione/process group;
- parent harness terminato con SIGTERM e SIGKILL mentre il workload resta
  attivo.

S3 non richiede il daemon Termux Stacks. La strategia scelta viene ripetuta
sotto il daemon reale in S5, dove entrano in gioco supervisione e recovery.

Confrontare:

- segnale al child/PGID host;
- `proot-distro kill <session-pid|alias>`, dove il PID è quello restituito
  da `proot-distro ps`;
- escalation engine;
- orfani dopo stop.

Eseguire almeno 100 cicli sul workload più semplice. Il test fallisce se
rimane un guest osservabile o viene segnalato un PID estraneo.

Exit:

- una sola strategia di stop v0;
- timeout fisso documentato come best effort;
- nessun `stopGracePeriod` pubblico se il segnale applicativo non è
  generalizzabile.

## 9. Fase S4 — Ownership e crash durante install

**Obiettivo:** classificare ciò che lascia un `proot-distro install`
interrotto, prima di progettare il confine SQLite/engine.

Il test usa soltanto la CLI pubblica dell'engine e alias disposable, casuali e
mai riutilizzati. Interrompe `proot-distro install` in finestre controllate
durante download ed estrazione, quindi classifica l'alias risultante:

- `absent`: l'alias non è osservabile;
- `owned`: l'alias disposable è osservabile e attribuibile a quel tentativo;
- `ambiguous`: le interfacce pubbliche non bastano a provare uno dei due casi.

S4 non introduce SQLite, daemon, start del workload o commit di revisione. Il
test non usa né modifica alias preesistenti. Intent persistito e fault point
transazionali vengono applicati nel vertical slice S5 usando questa tabella di
esiti.

Exit:

- tabella raw + golden degli esiti `absent | owned | ambiguous`;
- strategia deterministica futura per `absent` e `owned`;
- `ambiguous` definito fail-closed: nessuna cancellazione o avvio automatico;
- nessun test richiede accesso agli internals di `proot-distro`;
- procedura manuale per gli artefatti dubbi.

## 10. Fase S5 — Vertical slice

**Obiettivo:** un percorso end-to-end utile, non ancora multi-servizio.

Deliverable:

- parser strict del profilo vertical slice;
- parser production dell'output engine derivato dal corpus raw + golden S2;
- daemon singleton tramite lock advisory e socket locale;
- protocollo request/response a versione esatta;
- SQLite `meta/stacks/services/operations`;
- `validate/up/status/down`;
- install rootfs, run foreground, log ed exit;
- reconciliation definita dagli esiti S2–S4;
- fake engine per test host e adapter reale su device;
- ripetizione sotto il daemon reale dei test signal/tree-kill scelti in S3;
- upgrade del binario mentre il daemon precedente è vivo: protocol mismatch
  diagnosticato senza proseguire.

Fault points obbligatori:

1. prima dell'intent;
2. dopo intent, prima dell'engine;
3. dopo install;
4. dopo start;
5. prima del commit;
6. durante down.

Exit:

- ciclo completo ripetibile su aarch64;
- 20 kill/restart senza duplicati **oppure** passaggio sicuro a `unknown`
  quando l'assenza non è dimostrabile;
- database consistente dopo kill -9 e storage full simulato;
- log non bloccano il child;
- campi MVP non implementati falliscono come `unsupported`.

## 11. Fase M1 — MVP multi-servizio

Si apre solo dopo S0–S5.

Vertical slice incrementali, ciascuno completo di test e recovery:

1. più stack e namespace;
2. più servizi e ordinamento DAG;
3. environment letterale;
4. volumi nominati e bind;
5. porte loopback fisse;
6. restart/backoff;
7. `logs --tail` e `restart`.

Non si sviluppano due slice in parallelo se condividono una failure mode non
ancora chiusa. Ogni nuova colonna SQLite e ogni stato deve corrispondere a una
domanda utente o di recovery concreta.

## 12. CI proporzionata

Controlli proporzionati per fase:

| Trigger | Controlli |
|---|---|
| ogni change, S0–S4 | fmt, Clippy, unit e validazione di harness/corpus spike |
| ogni change, da S5 | controlli precedenti, parser fixture e fake-engine contract |
| main/dipendenze | test integrazione host e build source archive |
| nightly o crate native | package build sulle 4 architetture Termux |
| manuale | smoke e fault test su un device aarch64 |

Prima dell'RC si aggiungono più versioni Android, almeno un secondo device,
reboot/Doze/storage pressure e soak. Tre device e 72 ore non bloccano il
bootstrap.

Ogni failure device conserva versioni, argv redatti, DB integrity result,
operazione corrente, process tree e log. I test non devono esportare secret.

## 13. Gate

### G0 — Feasibility

S0–S4 completate; command, sessioni, signal e ownership hanno un contratto
verificato. Un fallimento riduce scope o ferma il progetto prima del daemon.

### G1 — Vertical slice

S5 completa su aarch64 e recovery coerente con le garanzie pubbliche.

### G2 — MVP

M1 completa: due stack, almeno due servizi, DAG, volume, porta e restart
superano smoke e fault test.

### G3 — Package candidate

Build quattro architetture, licenza, release archive/checksum, service
disabilitato, upgrade testato e feedback sul nome acquisito.

### G4 — Official proposal

Il progetto è attivo, ha utenti/release e soddisfa la policy vigente. Essere
tecnicamente pacchettizzabile non garantisce l'ammissione.

## 14. Rischi residui

| Rischio | Risposta |
|---|---|
| sessione non registrata | fail closed, niente auto-start |
| PID riutilizzato | evidenze multiple, mai PID-only |
| crash fra SQLite ed engine | intent-first, alias unico, classify |
| rootfs parziale | conserva e segnala, cleanup manuale |
| tag OCI mutabile/cache | niente promessa di update riproducibile |
| Android kill/force-stop | runit/Boot best effort, limite pubblico |
| disk full/corruzione | test fault, stop delle mutazioni |
| output engine cambia | adapter + fixture, fail closed |
| crate native non cross-build | spike e fallback prima dell'MVP |
| scope creep | feature differite richiedono gate/ADR |

## 15. Definition of Done v0.1

v0.1 è completa quando un utente può:

1. installare il package senza toolchain Rust sul dispositivo;
2. abilitare esplicitamente il servizio;
3. validare e avviare due stack multi-servizio;
4. consultare stato e log, riavviare e fermare;
5. conservare dati in un volume attraverso restart e nuova immagine;
6. ricevere un errore su porta occupata o feature non supportata;
7. uccidere e riavviare il demone senza duplicazione silenziosa;
8. capire quando serve intervento manuale;
9. comprendere che PRoot non è isolamento e Android resta best effort.

Python può essere presente come dipendenza transitiva di `proot-distro`.
La garanzia è che Termux Stacks non installa un runtime Rust o un secondo
package manager durante installazione/avvio.
