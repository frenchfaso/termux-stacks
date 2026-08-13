# Specifica del prodotto Termux Stacks

**Stato:** proposta v0.1; bootstrap S0 completato, spike S1–S4 aperti
**Target:** Termux su Android senza root
**Autorità:** comportamento pubblico del prodotto

## 1. Definizione

Termux Stacks è un orchestratore locale dichiarativo. Porta più stack di
servizi verso lo stato desiderato su una singola installazione Termux usando
`proot-distro` come engine PRoot e runit per mantenere attivo il control
plane.

Termux Stacks aggiunge ciò che manca fra un singolo rootfs PRoot e
un'esperienza Compose essenziale:

- un manifest multi-servizio;
- lifecycle `up`, `down`, `status`, `logs` e `restart`;
- dipendenze di avvio semplici;
- rootfs separati, volumi espliciti e log per servizio;
- restart con backoff;
- stato durevole e recovery dopo crash del control plane;
- diagnostica onesta dei limiti Android.

Non reimplementa l'acquisizione OCI durante `install`, l'estrazione del
rootfs o il tree-kill: li delega alle interfacce pubbliche di
`proot-distro`.

## 2. Scope v0.1

Il vertical slice iniziale supporta un solo stack con un solo servizio. È un
gate tecnico e non viene presentato come MVP.

L'MVP v0.1 DEVE poi supportare:

1. più stack indipendenti sullo stesso dispositivo;
2. più servizi per stack, una replica per servizio;
3. un grafo aciclico `dependsOn` che ordina l'avvio;
4. immagini OCI installabili da `proot-distro`;
5. `command` come override del Cmd OCI, senza override dell'Entrypoint;
6. environment letterale;
7. bind mount espliciti e volumi nominati;
8. porte TCP fisse su `127.0.0.1`, dichiarative e senza NAT;
9. restart `no`, `on-failure` e `always`, con backoff limitato;
10. log su file ed exit status per servizio;
11. stato desiderato `running | stopped`;
12. recovery conservativa: restart solo quando assenza o target sono provati,
    altrimenti `unknown` con diagnostica per l'intervento manuale;
13. installazione del servizio runit disabilitata per default;
14. errori espliciti per campi o capability non supportati.

## 3. Non-obiettivi

Termux Stacks v0.1 NON promette:

- isolamento di sicurezza fra servizi;
- UID, PID, mount, IPC, UTS o network namespace distinti;
- cgroup o limiti rigidi di CPU, RAM, I/O e processi;
- firewall, DNS per servizio, IP virtuali, NAT o port mapping;
- mount realmente read-only;
- compatibilità completa con Docker, Dockerfile o Compose;
- più repliche per servizio;
- job, migration hook o semantica exactly-once;
- build OCI;
- porte automatiche o service discovery tipizzata;
- secret manager, config manager, cache manager o backup generico;
- readiness/liveness/startup probe separate;
- update atomici, zero downtime o rollback dati;
- funzionamento dopo un force-stop Android;
- disponibilità continua sotto Doze, pressione memoria o policy OEM;
- orchestrazione fra dispositivi.

Queste funzioni richiedono un nuovo milestone e, quando cambiano il contratto,
un ADR. Non devono essere anticipate con placeholder pubblici.

## 4. Modello minimo

```text
Stack
├── Revision
├── Service
│   └── RootfsGeneration
└── Volume

Operation registra intent e risultato di una mutazione.
```

### Stack

È il namespace e l'unità di lifecycle. Il nome è univoco nell'installazione.

### Revision

È una versione sequenziale del manifest accettato dal demone. v0.1 non espone
digest canonicali, lockfile o revisioni portabili. Una nuova configurazione
crea una nuova revisione; il commit avviene solo dopo l'avvio riuscito dei
servizi richiesti.

### Service

Descrive un processo long-running. Possiede un rootfs; il demone non ne avvia
una nuova sessione senza avere escluso una precedente. Processi indipendenti
devono essere servizi distinti.

### RootfsGeneration

È un container `proot-distro` con alias Termux Stacks. Non è immutabile:
il guest può scriverci. Viene riusato nei restart dello stesso servizio e
sostituito quando cambia l'immagine. Due servizi non condividono mai lo stesso
rootfs scrivibile.

### Operation

Registra almeno stack, tipo, fase, revisione candidata, timestamp e risultato.
L'intent viene committato prima dell'effetto esterno. Non esiste un secondo
journal autorevole fuori da SQLite.

## 5. Invarianti

Il runtime DEVE mantenere:

1. un solo demone mutante per installazione;
2. una sola mutazione globale in corso;
3. il demone non avvia intenzionalmente una seconda sessione per
   stack/servizio e non riavvia quando l'assenza non è provata;
4. un rootfs scrivibile appartenente a un solo servizio;
5. nessuna rimozione automatica di un rootfs con ownership ambigua;
6. intent persistito prima di install, start o stop;
7. nessuna transazione SQLite aperta mentre attende un comando engine;
8. PID non usato da solo come prova d'identità;
9. porta fissa in conflitto trattata come errore;
10. stato ambiguo trattato come `unknown`, mai come successo;
11. persistenza garantita solo per volumi e bind dichiarati;
12. nessuna API viene descritta come secret-safe se può esporre il valore.

## 6. Lifecycle

### up

`up` valida il manifest, persiste una nuova operazione, prepara i rootfs,
ferma i servizi sostituiti, avvia la nuova configurazione e infine committa la
revisione.

```text
PREPARE -> STOP_OLD -> START_NEW -> COMMIT
```

v0.1 ammette downtime. Se fallisce prima del commit, il demone tenta di
ripristinare l'ultima revisione committed. Se l'osservazione è ambigua, marca
lo stack `unknown` e non procede automaticamente.

### down

`down` persiste `desired=stopped`, termina in ordine inverso i servizi e
conserva rootfs, log e volumi. La rimozione permanente non fa parte di v0.1.

```text
STOP_REQUESTED -> STOPPING -> STOPPED
```

Dopo un crash, qualunque fase incompleta conserva `desired=stopped`: il
demone riprende lo stop solo per target con identità provata.

### restart

`restart` riavvia il processo sullo stesso rootfs. Non crea una revisione e
non è un metodo di aggiornamento.

```text
RESTART_REQUESTED -> STOPPING -> STARTING -> RUNNING
```

Dopo un crash valgono le stesse regole conservative: nessun nuovo start se
l'assenza della sessione precedente non è provata.

## 7. Stato osservato e restart

Uno stack è `stopped`, `starting`, `running`, `failed` o `unknown`.
Un servizio è `absent`, `starting`, `running`, `stopping`,
`stopped`, `backoff`, `failed` o `unknown`.

La derivazione dello stack è deterministica: `unknown` se un servizio è
ambiguo; `stopped` se lo stato desiderato è stopped e nessuna sessione è
attiva; `running` se tutti i servizi richiesti sono running; `failed` se
un servizio richiesto ha esaurito la restart policy; `starting` negli altri
casi di convergenza.

`absent` indica che non esiste un rootfs registrato; `stopped` che il
rootfs esiste ma il processo non è attivo.

`running` significa che il processo principale è osservato, non che
l'applicazione sia pronta. Una porta dichiarata può essere controllata per
raggiungibilità, ma v0.1 non attribuisce in modo affidabile la proprietà del
listener a uno specifico processo PRoot.

Il backoff deve avere limite massimo e finestra anti crash-loop. I default
sono dettagli di implementazione finché non vengono misurati su device.

## 8. Rete e storage

Tutti i servizi usano la rete Android condivisa. La comunicazione locale usa
`127.0.0.1:<porta-fissa>`; il manifest deve configurare l'applicazione perché
ascolti realmente su quella porta. La dichiarazione `ports` dichiara e
verifica best effort, non riserva e non effettua mapping.

Un volume nominato vive sotto lo stato privato Termux Stacks e sopravvive a
restart, nuova immagine e `down`. Un bind monta un percorso host esplicito.
Lo storage Android condiviso non viene usato implicitamente.

## 9. CLI v0.1

Vertical slice:

- `config validate FILE`;
- `up FILE`;
- `status STACK`;
- `down STACK`.

MVP:

- `logs STACK SERVICE [--tail N]`;
- `restart STACK SERVICE`.

`logs` restituisce al massimo 200 righe per default e rifiuta una risposta
oltre il limite del protocollo. v0.1 non offre follow/streaming.

L'abilitazione del service resta a `sv-enable termux-stacksd`; stato e log
del control plane restano agli strumenti `sv`/runit. `status` mostra stack
e servizi, quindi non esiste un secondo comando `ps`.

Le mutazioni passano sempre dal demone. `config validate` è l'unico comando
garantito offline; non esegue capability probe, pull o controllo delle porte.
Il demone può rifiutare un manifest sintatticamente valido dopo i preflight.

Non sono contratti v0.1: `plan`, `lock`, `pull`, `exec`, `run`,
`update`, `rollback`, `events`, `backup`, `restore` e `gc`.

## 10. Garanzie e limiti Android

| Proprietà | Garanzia v0.1 |
|---|---|
| isolamento fra workload | nessuna |
| nessun avvio duplicato intenzionale | il demone riavvia solo quando l'assenza precedente è provata |
| recovery crash demone | stop/recreate solo con evidenza sufficiente; altrimenti `unknown` |
| persistenza | solo volumi e bind dichiarati |
| rete privata | nessuna |
| esposizione default | solo dichiarazioni loopback |
| reboot | best effort se runit/Termux:Boot sono configurati |
| force-stop | richiede riapertura manuale di Termux |
| update | stop/recreate con downtime |
| rollback | non incluso in v0.1 |

I workload devono essere fidati: condividono l'UID Android di Termux e possono
accedere alle risorse leggibili da quell'UID.

## 11. Criterio di successo

v0.1 è pronta soltanto quando più stack multi-servizio eseguono cicli
`up/status/logs/restart/down` su Android aarch64, i volumi sopravvivono,
nessun rootfs è condiviso e una campagna di kill/restart del demone non crea
duplicati osservabili. Un limite dell'engine che impedisca questa proprietà
deve ridurre la garanzia pubblica o fermare il milestone.
