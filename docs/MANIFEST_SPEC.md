# Specifica del manifest Termux Stacks

**Stato:** proposta v0.1; parser non ancora implementato
**Schema:** `termux-stacks/v1alpha1`
**Autorità:** sintassi e semantica del manifest

## 1. Principi

Il manifest descrive ciò che Termux/PRoot può applicare davvero. Non copia
campi Compose privi di semantica affidabile su Android.

Il parser DEVE:

- accettare un solo documento YAML;
- rifiutare tag custom, merge key, anchor, alias e chiavi duplicate;
- rifiutare campi sconosciuti;
- imporre limiti a file, nesting, collezioni e scalari;
- produrre errori con percorso del campo e, quando disponibile, linea/colonna;
- non eseguire interpolazione, include o comandi.

Non esistono in v0.1 variabili, estensioni `x-*`, canonicalizzazione pubblica,
digest del manifest o lockfile.

## 2. Profilo vertical slice

Il primo slice accetta esattamente un servizio e questi soli campi:

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

La semantica di `command` è stata verificata dallo spike S1 su
`proot-distro 5.6.0`. È un override del solo OCI `Cmd`: non sostituisce
`Entrypoint` e non è un raw exec indipendente dall'immagine. Il runtime deve
fallire chiuso se il capability probe non conferma questo contratto.

Qualunque campo MVP non ancora implementato deve produrre `unsupported`, non
essere ignorato.

## 3. Schema MVP

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

Campi top-level ammessi:

| Campo | Tipo | Obbligatorio |
|---|---|---|
| `apiVersion` | stringa esatta | sì |
| `kind` | stringa esatta `Stack` | sì |
| `metadata.name` | nome | sì |
| `services` | mappa non vuota | sì |
| `volumes` | mappa | no |

## 4. Nomi

Nomi di stack, servizio e volume:

- corrispondono a `^[a-z][a-z0-9-]{0,47}$`;
- sono case-sensitive ma ammettono solo minuscole;
- non possono iniziare con `termux-stacks-`;
- sono univoci nel proprio namespace.

Il runtime non usa il nome direttamente come path o alias engine: applica
escaping e un identificatore d'installazione.

## 5. Servizi

Ogni servizio ammette:

| Campo | Tipo | Default |
|---|---|---|
| `image` | stringa non vuota | obbligatorio |
| `command` | array non vuoto di stringhe, primo elemento non vuoto | assente: comando OCI |
| `environment` | mappa stringa→stringa | `{}` |
| `mounts` | array di mount | `[]` |
| `ports` | array di porta | `[]` |
| `dependsOn` | array di nomi servizio | `[]` |
| `restart` | enum | `no` |

Un servizio rappresenta un processo foreground. Se l'applicazione daemonizza
e il processo principale termina, il servizio è terminato.

## 6. Immagine e command

`image` identifica un'immagine da registry OCI oppure un OCI image archive
locale accettato da `proot-distro install`. Un rootfs tar plain è rifiutato:
non contiene il manifest richiesto da `proot-distro run`. v0.1 non offre una
policy di pull, non promette risoluzione a digest e non modifica la cache
globale di immagini per forzare un refresh.

Il percorso raccomandato per test riproducibili è un OCI archive locale o un
tag immutabile controllato. Tag mutabili sono accettabili solo dichiarando che
una successiva installazione può usare la cache.

La CLI pubblica di `proot-distro`, verificata su device in S1, definisce
questa semantica:

- se `command` è assente, l'adapter omette `--`: `run` esegue Entrypoint +
  Cmd, il solo Cmd o il solo Entrypoint; fallisce se entrambi mancano;
- se `command` è presente, l'adapter emette un solo `--` seguito dagli
  elementi distinti: l'array sostituisce Cmd ma conserva Entrypoint;
- senza Entrypoint, il primo elemento non vuoto di `command` è il programma;
- `command: []` è invalido: `run ALIAS --` equivale a nessun override e non
  può rappresentare “svuota Cmd”;
- v0.1 non offre né clear-Cmd né override generico dell'Entrypoint.

L'adapter costruisce un vettore argv: non concatena, interpreta, espande né
aggiunge una shell. Spazi, stringhe vuote successive al primo elemento,
metacaratteri e argomenti che iniziano con `-` restano letterali. L'immagine
può naturalmente scegliere una shell nel proprio Entrypoint/Cmd o shebang.

v0.1 non espone `workingDirectory`; l'adapter non passa `--work-dir`. Resta
quindi attivo l'OCI `WorkingDir`, con fallback engine a `/`.

## 7. Environment

`environment` contiene solo valori letterali. Nomi variabile:
`^[A-Za-z_][A-Za-z0-9_]*$`.

v0.1 rifiuta le chiavi che `proot-distro` filtra, riscrive o usa per il
proprio funzionamento:

```text
ANDROID_ART_ROOT ANDROID_DATA ANDROID_I18N_ROOT ANDROID_ROOT
ANDROID_RUNTIME_ROOT ANDROID_TZDATA_ROOT BOOTCLASSPATH
DEX2OATBOOTCLASSPATH EXTERNAL_STORAGE HOME USER TERM COLORTERM PREFIX TMPDIR
MOZ_FAKE_NO_SANDBOX PULSE_SERVER
```

Sono inoltre riservate tutte le chiavi che iniziano con `PROOT_` o `LD_`,
incluse quelle non note alla versione corrente dell'adapter.

Il capability profile può estendere questa lista per una futura versione
engine, mai ridurla silenziosamente. La restrizione conserva la stessa
semantica fra immagini Linux normali e rootfs riconosciuti come Termux.

Secret, riferimenti a file, interpolazione host e `fromEndpoint` non sono
supportati. In particolare, la CLI pubblica dell'engine passa `--env K=V`
nel proprio argv host; quindi v0.1 non accetta una funzionalità secret che
prometterebbe di non apparire negli argv.

Ogni coppia letterale non riservata viene passata come un distinto
`--env K=V`, senza shell. Per queste chiavi il valore del manifest sostituisce
l'omonimo OCI `Env`; le altre variabili OCI non riservate rimangono
disponibili. S1 ha verificato override e aggiunta con due chiavi ordinarie;
non generalizza il risultato alle chiavi gestite dall'engine.

## 8. Mount e volumi

Mount volume:

```yaml
mounts:
  - type: volume
    source: data
    target: /data
```

Mount bind:

```yaml
mounts:
  - type: bind
    source: ./config
    target: /app/config
```

Regole:

- `type` è `volume` o `bind`;
- `target` è assoluto, normalizzato e non contiene `..`;
- un volume deve essere dichiarato top-level;
- un bind relativo viene risolto rispetto alla directory del manifest;
- la sorgente deve esistere; la destinazione viene validata nei limiti
  esposti dall'engine;
- destinazioni duplicate o sovrapposte vengono rifiutate;
- non esiste un flag `readOnly`: PRoot non lo garantisce.

Un volume dichiarato è una directory privata gestita da Termux Stacks. Il
manifest v0.1 non espone driver, quota, backup o lifecycle policy.

## 9. Porte

```yaml
ports:
  - address: 127.0.0.1
    port: 8080
```

Solo `127.0.0.1` è ammesso e `port` è un intero 1024–65535. Una porta può
essere dichiarata da un solo servizio del manifest. Il demone esegue un
preflight best effort; una collisione produce errore e non viene riallocata.

Questo campo non configura il processo, non crea NAT e non prova ownership del
listener. Il comando o environment del workload deve usare la stessa porta.
Porte automatiche, LAN, socket Unix dichiarativi e discovery sono differiti.

## 10. Dipendenze

`dependsOn` è un array di servizi. Il grafo deve essere aciclico. L'avvio di
un servizio dipendente attende che il processo del predecessore sia osservato
vivo; non implica readiness applicativa. Lo stop usa l'ordine inverso.

Un failure del predecessore prima dell'avvio impedisce l'avvio dei dipendenti
e rende lo stack `failed`. Dopo che il dipendente è partito, un exit del
predecessore non lo arresta automaticamente; la restart policy del
predecessore opera indipendentemente.

## 11. Restart

Valori ammessi:

- `no`: nessun restart automatico;
- `on-failure`: restart solo per exit non zero o segnale;
- `always`: restart finché lo stato desiderato è `running`.

Backoff, finestra e limite sono inizialmente default interni misurati su
device. Non sono ancora configurabili nel manifest.

## 12. Validazione

`config validate` esegue offline:

- parsing ristretto e schema;
- nomi e riferimenti;
- `command` assente oppure array non vuoto con primo elemento non vuoto;
- nomi environment validi e non riservati all'engine;
- path e tipi verificabili senza effetti;
- ciclo delle dipendenze;
- conflitti interni fra mount e porte.

`up` aggiunge lato demone:

- capability engine;
- presenza/forma dell'immagine;
- compatibilità della command matrix;
- accesso ai path host;
- conflitti con altri stack e stato runtime;
- spazio disponibile e preparazione del rootfs.

Un validate offline riuscito non garantisce che `up` riesca.

## 13. Evoluzione

`v1alpha1` può cambiare in modo incompatibile prima della prima release.
L'implementazione rifiuta versioni sconosciute e campi futuri; non conserva
silenziosamente dati che non sa applicare.

Job, build, config/secret, cache, probe, endpoint automatici, lockfile,
policy, replica, update hook/policy e rollback richiederanno estensioni
esplicite dello schema dopo che il lifecycle minimo sarà validato.
