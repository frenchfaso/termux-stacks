# Device harness

Gli harness raccolgono evidenze per i gate eseguiti su Termux reale. S0 prova
binario/package/runit; S1 qualifica la semantica pubblica di
`proot-distro run`. Sono separati dagli adapter production e non richiedono il
daemon Termux Stacks, salvo il test isolato dello scaffold in S0.

## S0 — Bootstrap

Questo harness raccoglie evidenze ripetibili per il checkpoint S0 su un
dispositivo Termux. Verifica un binario `termux-stacks` **già fornito**: non
compila, non installa package, non esegue `apt`/`pkg`, non abilita servizi e non
usa `sudo`.

## Prerequisiti

- Termux con Bash e coreutils;
- un binario `termux-stacks` eseguibile per l'architettura del dispositivo;
- `PREFIX` impostato dalla sessione Termux;
- `file` e `readelf` sono opzionali: i controlli corrispondenti diventano
  `SKIP` quando il tool non è disponibile.

Il package `termux-stacks` e l'integrazione runit non sono prerequisiti. I loro
controlli sono read-only e diventano `SKIP` se il package non è installato.

## Uso

Esecuzione minima:

```bash
bash tests/device/s0.sh --binary /percorso/al/binario/termux-stacks
```

Per scegliere la directory **base** delle evidenze:

```bash
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/s0.sh \
  --binary /percorso/al/binario/termux-stacks \
  --output-root "$HOME/termux-stacks-evidence"
```

`--output-root` deve indicare una directory assoluta, esistente e scrivibile.
Senza l'opzione viene usato `TMPDIR`. In entrambi i casi lo script crea con
`mktemp` una directory privata e univoca `termux-stacks-s0.*`; non riusa né
sovrascrive una directory precedente.

Il daemon viene eseguito con un `PREFIX` sintetico sotto il workspace privato
dell'harness. Il test non crea `state.db`, lock o socket sotto il vero
`$PREFIX/var` del dispositivo. I segnali TERM e KILL sono inviati soltanto ai
processi figli avviati dallo script.

## Controlli

S0 copre:

1. inventario di Termux, Android, architettura, filesystem e package rilevanti;
2. `termux-stacks --version` e `--help`;
3. SHA-256 del binario e, se disponibili, `file` e `readelf`;
4. creazione dei path privati sotto il prefix sintetico e relativi mode;
5. esclusione di un secondo daemon tramite lock/socket;
6. rilascio del lock e recupero dello socket stale dopo TERM;
7. rilascio del lock e recupero dello socket stale dopo KILL;
8. ispezione read-only del package e del servizio runit, se installati.

Lo script `s0.sh` non contiene test S1-S4, immagini OCI o operazioni
`proot-distro`.

## Evidenze

La directory stampata al termine contiene:

```text
evidence/
├── metadata.tsv
├── results.tsv
├── stdout-stderr/
├── conclusions.md
└── SHA256SUMS
```

`results.tsv` usa gli stati `PASS`, `FAIL` e `SKIP`. Un `FAIL` produce exit
code `1`; soli `PASS`/`SKIP` producono exit code `0`. `conclusions.md` contiene
un riepilogo automatico e uno spazio per la revisione manuale. L'harness
conserva `evidence/` e rimuove soltanto il proprio sottoalbero `work/`.

## S1 — Entrypoint e Cmd

S1 attualmente richiede un device Termux aarch64, `proot-distro 5.6.0`, i
normali tool core Termux e l'immagine
`alpine:3.24.1` già visibile nella cache locale. Il preflight evita di
richiedere intenzionalmente un'immagine assente, ma l'inventario quiet non
prova architettura o completezza della cache e non certifica una build
offline. L'harness costruisce quattro fixture locali con
Entrypoint+Cmd, solo Cmd, solo Entrypoint e nessuno dei due:

```bash
mkdir -p "$HOME/termux-stacks-evidence"
bash tests/device/s1.sh --output-root "$HOME/termux-stacks-evidence"
```

Ogni esecuzione genera tag e alias casuali `txs-s1-*`, ne registra l'intent
prima della creazione e usa soltanto confronti exact-name. Il teardown prova
la baseline con le interfacce pubbliche, arresta/rimuove solo i propri alias e
rimuove solo i propri image reference. Non usa mai `clear-cache`, `reset`,
`remove --all`, glob o un alias preesistente. Se l'inventario diventa ambiguo,
fallisce senza ampliare il target di cleanup.

La matrice verifica default e override per i quattro shape, argv problematici
in forma esadecimale, working directory, environment, exit status e il confine
shell di `login`. Non prova registry delle sessioni o segnali: appartengono a
S2 e S3.
