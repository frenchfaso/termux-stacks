# Architettura di Termux Stacks

**Stato:** proposta v0.1; bootstrap S0 completato, spike S1–S4 aperti
**Target:** Termux/Android senza root
**Baseline da verificare:** `proot-distro 5.6.0`
**Autorità:** componenti interni, persistenza e recovery

## 1. Criterio di progetto

L'architettura ottimizza per affidabilità e comprensibilità su un singolo
telefono, non per scalabilità teorica. Ogni componente deve giustificare una
failure mode reale già osservata.

Regole:

1. un processo e una fonte di verità prima di introdurre coordinamento;
2. serializzare prima di parallelizzare;
3. delegare all'engine ciò che l'engine sa già fare;
4. persistere intent prima degli effetti esterni;
5. non automatizzare quando l'osservazione è ambigua;
6. nessuna astrazione pubblica per funzioni differite.

## 2. Decisioni congelate

- Un solo eseguibile pubblico Rust: `termux-stacks`.
- Un solo package/crate iniziale; i confini sono moduli Rust.
- Un solo demone globale, `termux-stacks daemon`, foreground sotto runit.
- `termux-stacksd` è il service ID runit, non un artefatto binario.
- CLI a vita breve e socket Unix locale.
- Una coda globale FIFO per tutte le mutazioni.
- SQLite è l'unica fonte di verità transazionale.
- Un lock file advisory tenuto aperto rende il demone singleton; il socket è
  soltanto IPC. Non esistono lock per stack.
- `proot-distro` è accessibile soltanto attraverso un adapter.
- Un rootfs scrivibile distinto per servizio.
- Cold recovery: stop/recreate soltanto con evidenza sufficiente; altrimenti
  `unknown` con diagnostica operativa.
- Implementazione sincrona; un runtime async richiede misure e un ADR.

## 3. Contesto

```text
CLI breve
   │ JSON locale, versione esatta
   ▼
termux-stacks daemon ───────────── runit
   ├── command queue (single writer)
   ├── manifest
   ├── SQLite: desired + operations
   ├── supervisor in-process
   └── adapter proot-distro
          ├── rootfs service A ── processo/log
          └── rootfs service B ── processo/log
```

Tutto vive nello stesso UID Android. Il diagramma mostra ownership software,
non isolamento kernel.

## 4. Struttura del codice

```text
src/
├── main.rs       # dispatch CLI/daemon
├── cli.rs        # argomenti e output
├── manifest.rs   # parse e validate
├── protocol.rs   # tipi IPC e framing
├── daemon.rs     # accept loop e coda mutazioni
├── store.rs      # SQLite e migrazioni
├── engine.rs     # trait + adapter proot-distro
├── supervisor.rs # child, log, exit e restart
├── reconcile.rs  # startup e recovery
└── paths.rs      # layout sotto PREFIX
```

Non si creano crate `domain`, `planner`, `control-plane`, `storage` o
`xtask` finché un confine non richiede build, dipendenze o ownership
indipendenti.

## 5. CLI e protocollo

`config validate` opera localmente. Tutte le letture runtime e le mutazioni
passano dal demone; la CLI non apre SQLite e non avvia workload.

Il protocollo v0 usa request/response JSON Lines su socket Unix:

- un oggetto JSON per riga, massimo 1 MiB;
- `protocol_version` esatta in ogni richiesta;
- `request_id` univoco per deduplicare retry;
- un solo risultato finale, senza streaming o cursor;
- incompatibilità di versione = errore con istruzione di riavviare il servizio.

Non esistono negotiation range, subscription, backpressure o API remota.
Il daemon già in esecuzione può essere più vecchio del nuovo binario dopo un
upgrade: la versione esatta impedisce che CLI e demone incompatibili
proseguano silenziosamente.

## 6. Demone e concorrenza

All'avvio il demone:

1. calcola i path dal prefix;
2. prepara i path senza seguire symlink;
3. acquisisce il lock advisory non bloccante del demone;
4. recupera un eventuale socket stale e binda il socket, senza accettare
   richieste;
5. apre SQLite e accetta solo la versione schema esatta;
6. esegue capability probe dell'engine;
7. riconcilia operazioni incomplete;
8. accetta richieste.

Il lock è rilasciato dal kernel alla chiusura del file descriptor, incluso il
crash. Il file può restare sul disco e non contiene stato autorevole. Solo chi
possiede il lock può sostituire un socket stale: il bind del socket da solo
non è un algoritmo di elezione sicuro.

La mutazione corrente è l'unico writer logico. Read e raccolta di exit status
possono usare thread limitati, ma ogni modifica allo stato rientra nella coda.
SQLite non rimane mai in transazione durante install, run, kill o I/O lungo.

Il demone riceve SIGTERM da runit, smette di accettare mutazioni, registra lo
shutdown e termina i workload secondo policy. Un kill -9 viene gestito solo al
riavvio.

## 7. Persistenza

Layout minimo:

```text
$PREFIX/
├── var/lib/termux-stacks/
│   ├── state.db
│   ├── volumes/<stack>/<volume>/
│   └── logs/<stack>/<service>.log
├── var/run/termux-stacks/
│   ├── daemon.lock
│   └── daemon.sock
└── var/service/termux-stacksd/
```

`state.db` contiene concettualmente:

- `meta`: schema e installation ID;
- `stacks`: desired state, manifest accettato, revisione committed;
- `services`: alias engine, rootfs generation, stato e ultimo exit;
- `operations`: request ID, intent, fase e outcome.

Queste quattro tabelle sono un punto di partenza, non uno schema pubblico.
`operations` è il journal. Non esistono journal file, snapshot, `current`,
event store o compaction separati.

Prima del primo upgrade di formato supportato, il daemon crea solo database
vuoti e non modifica uno schema sconosciuto: conserva il file e termina con
diagnostica. Un framework di migrazione verrà introdotto soltanto quando
esisterà una migrazione reale da supportare.

Durabilità iniziale:

- transazioni SQLite e foreign key abilitate;
- binding, journal mode e `synchronous` scelti dallo spike su filesystem
  Termux;
- intent committato prima di ogni effetto;
- outcome committato dopo aver osservato l'effetto;
- errore storage/full trattato prima di proseguire.

## 8. Contratto engine

L'adapter usa soltanto i comandi pubblici `proot-distro` e non legge o
modifica direttamente i suoi moduli Python, database o rootfs internals.

Operazioni v0:

- capability probe;
- install di immagine/archive con alias;
- run foreground;
- list/ps delle sessioni;
- kill per PID radice restituito da `proot-distro ps` o alias registrato;

`--detach` è vietato: scarta stdio e sottrae il processo alla supervisione
diretta. L'adapter deve catturare stdout, stderr ed exit status.

### 8.1 Limiti da verificare

Il registry delle sessioni engine è best effort. Un `ps` vuoto non è prova
sufficiente che nessun processo esista se la registrazione può essere
fallita. Lo spike deve testare il filesystem reale, failure di `flock` e
crash; finché non passa, la recovery automatica non promette assenza di
duplicati.

`run CONTAINER -- ARGS` conserva Entrypoint e sostituisce Cmd. `login --`
passa invece da una login shell. L'adapter non inventa un raw exec generico:
supporta la semantica verificata e rifiuta il resto.

`kill` esegue tree-kill ed escalation propria. La propagazione di TERM al
guest, PGID e processi figli deve essere misurata. v0.1 non espone
`stopGracePeriod` configurabile finché una grace applicativa non è
realizzabile in modo generale.

## 9. Identità e ownership

Ogni installazione genera un installation ID casuale. Ogni tentativo di creare
un rootfs usa un alias non riutilizzabile:

```text
txs-<installation-short>-<stack-short>-<service-short>-<random>
```

L'intent con alias viene committato prima di invocare l'engine. Un prefisso da
solo non prova ownership e un alias incompleto non viene cancellato
automaticamente.

Per un processo servono almeno alias engine, PID osservato, start time quando
disponibile e boot identity. Un PID salvato non autorizza da solo un segnale.
`ps` engine e child handle sono evidenze complementari, non infallibili.

## 10. Lifecycle di un servizio

### Prepare

Questa procedura viene eseguita soltanto se il rootfs manca o cambia
l'immagine. Un restart riusa il rootfs registrato.

1. valida manifest e capability;
2. genera alias non riutilizzabile;
3. inserisce operation `PREPARE`;
4. invoca install;
5. osserva successo e registra il rootfs.

Un crash fra 3 e 5 lascia un'operazione incompleta. La recovery classifica
l'artefatto `absent | owned | ambiguous` e non lo cancella nel terzo caso.

### Start

1. registra `START_NEW`;
2. apre il file log;
3. avvia `proot-distro run` foreground;
4. registra child/session evidence;
5. considera il servizio `running` quando il processo principale è
   osservato vivo;
6. committa la revisione quando tutti i servizi sono running.

La readiness applicativa è fuori v0.1.

### Stop

L'adapter prova il percorso engine verificato dallo spike e attende la sua
escalation. Se non può provare l'identità, marca `unknown` e non invia segnali
a PID host potenzialmente riciclati. L'utente riceve una diagnosi e una
procedura manuale.

## 11. Reconciliation

All'avvio:

1. legge revisione committed, desired state e operazioni incomplete;
2. interroga child/session evidence disponibile;
3. classifica la sessione `absent | active | ambiguous`;
4. per `desired=stopped`, ferma solo target con ownership sufficiente;
5. per `desired=running`, riavvia solo dopo aver escluso un duplicato;
6. in caso ambiguo usa `unknown` con diagnostica operativa;
7. non rimuove rootfs automaticamente.

La strategia v0.1 è stop-and-recreate quando l'identità è provata, non
adozione. Se lo spike dimostra che
il session registry può fallire senza segnale osservabile, l'auto-restart dopo
crash resta disabilitato e la specifica viene ridotta prima dell'MVP.

## 12. Update

`up` con manifest cambiato usa:

```text
PREPARE -> STOP_OLD -> START_NEW -> COMMIT
```

Non è una transazione atomica. È consentito downtime. Se START_NEW fallisce,
si tenta di riavviare l'ultima revisione committed sul suo rootfs ancora
presente. Non esistono rollback pubblico, data migration o GC automatico in
v0.1; rootfs ritirati restano per cleanup manuale.

## 13. Networking e mount

Il demone controlla conflitti fra manifest e prova best effort se una porta è
libera prima dell'avvio. Non mantiene socket lease e non attribuisce ownership
del listener. Una race fra preflight e bind resta possibile.

I mount sono passati come bind pubblici dell'engine. Il daemon canonicalizza
i path host, vieta destinazioni sovrapposte e non promette read-only.

## 14. Runit e Android

La recipe installa un solo servizio:

```sh
exec "$PREFIX/bin/termux-stacks" daemon 2>&1
```

Il file `down` lo lascia disabilitato. L'utente usa
`sv-enable termux-stacksd`; l'abilitazione non viene eseguita da post-install.

Termux:Boot è opzionale ed esegue script one-shot. La configurazione
raccomandata avvia l'infrastruttura `termux-services`, non Termux Stacks
direttamente. Dopo un force-stop Android nessun componente può riavviarsi
finché l'utente non riapre Termux. Reboot, Doze e kill OEM restano best effort.

## 15. Sicurezza

Socket, database, log e volumi devono essere privati all'UID Termux. Il demone
non accetta path in shared storage per il proprio stato e non ascolta su TCP.

I workload sono fidati e possono vedere risorse dello stesso UID. v0.1 non
accetta secret come feature: `--env VALUE` può apparire negli argv e i bind
file non costituiscono isolamento dal workload.

## 16. Evoluzioni differite

Richiedono evidenza e ADR separati:

- più writer o mutazioni concorrenti;
- runtime async;
- split in più crate o runner separato;
- protocol negotiation e streaming;
- planner/lockfile/content addressing;
- job e migration;
- config, secret, cache e backup manager;
- auto-port, endpoint discovery, LAN e socket dichiarativi;
- probe avanzate;
- update a più fasi, rollback e GC;
- build OCI, import Compose ed exec interattivo.

## 17. Fonti ufficiali

- [PRoot-Distro v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/README.md)
- [Session registry v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/proot_distro/session.py)
- [Tree-kill v5.6.0](https://github.com/termux/proot-distro/blob/v5.6.0/proot_distro/commands/kill.py)
- [termux-services](https://github.com/termux/termux-services)
- [Termux:Boot](https://github.com/termux/termux-boot)
