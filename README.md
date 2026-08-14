# Termux Stacks

Termux Stacks è un orchestratore locale di applicazioni multi-servizio per
Termux/Android. L'esperienza è ispirata a Docker Compose, ma l'esecuzione usa
le primitive realmente disponibili senza root: `proot-distro` per immagini
OCI, rootfs e processi PRoot; `termux-services`/runit per il control plane.

Non è un container runtime del kernel. I servizi condividono UID Android,
kernel e rete con Termux: non esistono namespace, cgroup, firewall, mount
realmente read-only o isolamento fra workload ostili.

## Stato

Il bootstrap **S0 è completato su aarch64**. CI host, package Android e
servizio runit sono verdi; l'harness device v3 ha chiuso con 24 PASS, 0 FAIL
e 0 SKIP. Il ciclo stateful ha verificato enable/start, singleton, recupero
dello socket stale, restart dopo SIGKILL e disable finale. Il record
riproducibile è in [docs/evidence/S0.md](docs/evidence/S0.md).
Anche lo spike **S1 è completato**: 31 PASS hanno qualificato la composizione
OCI di `Entrypoint`/`Cmd`, argv, working directory, environment ordinario ed
exit status; il record è in [docs/evidence/S1.md](docs/evidence/S1.md). Lo
spike **S2 è completato**: 16 PASS hanno riprodotto tre falsi negativi del
session registry e congelato una policy fail-closed; il record è in
[docs/evidence/S2.md](docs/evidence/S2.md). Anche **S3 è completato**: la
strategia di stop usa soltanto l'identificatore esatto di sessione, ha drenato
tree cooperativi, TERM ignorato, un discendente in una nuova sessione e guest
rimasti dopo la morte del tracer PRoot, oltre a 100 cicli consecutivi. Il
record è in [docs/evidence/S3.md](docs/evidence/S3.md). Non esiste ancora un
runtime utilizzabile: l'ownership durante install deve superare S4 prima del
vertical slice S5.

La direzione architetturale è congelata solo nei punti essenziali:

- un package/crate Rust e un solo eseguibile pubblico, `termux-stacks`;
- una CLI breve e un demone globale avviato come `termux-stacks daemon`;
- `termux-stacksd` è il nome del servizio runit, non un secondo binario;
- un lock advisory del demone, un socket Unix locale e una sola coda di
  mutazioni;
- SQLite come unica fonte di verità, inclusi intent e risultati operativi;
- un adapter isolato che usa soltanto la CLI pubblica di `proot-distro`;
- un rootfs scrivibile distinto per servizio e persistenza solo esplicita;
- recovery conservativa: in caso di ambiguità, fermarsi e chiedere intervento.

In particolare, `proot-distro ps` vuoto non prova l'assenza di un workload.
Finché il demone conserva l'handle del figlio usa PID, start time e boot ID;
dopo la perdita di quell'handle uno stato non osservabile diventa `unknown` e
non autorizza restart, recreate o delete automatici.

Nella stessa generazione del demone, quando handle, identità persistita e
record positivo coincidono, lo stop v0 usa solo
`proot-distro kill <session-pid>`. Non segnala direttamente quel PID host, non
usa l'alias e non usa `--all`; l'exit status viene accettato soltanto insieme
alle precondizioni di ownership. La grace interna dell'engine resta fissa e
best effort, quindi il manifest non espone `stopGracePeriod`.

## Primo risultato utilizzabile

Il primo vertical slice supporta un solo servizio e soltanto:

```sh
# dopo aver installato termux-services e riavviato la shell
sv-enable termux-stacksd
termux-stacks config validate ./termux-stacks.yaml
termux-stacks up ./termux-stacks.yaml
termux-stacks status notes
termux-stacks down notes
```

Serve a validare il percorso completo manifest → demone → SQLite →
`proot-distro` → log → recovery. Il successivo MVP aggiunge più stack e più
servizi, dipendenze semplici, environment, volumi, porte loopback fisse,
restart e log. Job, secret manager, build, porte automatiche, update/rollback
avanzati e compatibilità Compose sono differiti.

Manifest MVP previsto:

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

`command` sostituisce il `Cmd` OCI e conserva l'eventuale `Entrypoint`, come
fa `proot-distro run CONTAINER -- ARG...`. `ports` non crea NAT: dichiara una
porta che l'applicazione deve realmente aprire sulla rete condivisa.

## Documentazione

- [Specifica del prodotto](docs/SPECIFICATION.md)
- [Specifica del manifest](docs/MANIFEST_SPEC.md)
- [Architettura](docs/ARCHITECTURE.md)
- [Piano di implementazione](docs/IMPLEMENTATION_PLAN.md)
- [Decisione Rust](docs/LANGUAGE_DECISION.md)
- [Packaging Termux](docs/TERMUX_PACKAGING.md)

Ogni argomento ha una sola fonte normativa: comportamento pubblico nella
specifica, schema nel manifest, dettagli interni nell'architettura e ordine
del lavoro nel piano.

## Dipendenze e limiti operativi

La baseline da verificare nello spike è `proot-distro 5.6.0`; il package
richiederà inoltre `termux-services`. Termux:Boot resterà opzionale e l'avvio
dopo reboot sarà best effort. Nessun processo può sopravvivere a un force-stop
Android dell'app Termux.

Lo stato rimane sotto `$PREFIX` e non nello storage Android condiviso. Il
servizio viene installato disabilitato: l'utente deve abilitarlo
esplicitamente.

## Nome, affiliazione e licenza

Gli identificatori pre-release sono:

- prodotto, repository e package: **Termux Stacks** / `termux-stacks`;
- CLI: `termux-stacks`;
- servizio runit: `termux-stacksd`;
- manifest: `termux-stacks.yaml`.

Termux Stacks è un progetto indipendente della comunità e non è approvato o
supportato dai maintainer di Termux. Prima della prima release pubblica va
chiesto un riscontro sull'uso di “Termux” nel nome.

Il progetto è distribuito sotto [Apache License 2.0](LICENSE). La licenza
riguarda Termux Stacks; `proot-distro`, Termux e le altre dipendenze
conservano le rispettive licenze.
