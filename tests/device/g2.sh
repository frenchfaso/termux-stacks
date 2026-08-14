#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
FIXTURE_DIR=$SCRIPT_DIR/fixtures/g2

DEVICE_PHASE=G2
DEVICE_RUN_LABEL=termux-stacks-g2
DEVICE_RUNTIME_LABEL=txs-g2
DEVICE_HARNESS_VERSION=4
DEVICE_AUTOMATIC_SCOPE=$'The harness exercised M1 with two concurrent two-service stacks and four\ncontrolled crash cases in fresh synthetic Termux Stacks and proot-distro\nprefixes. Every destructive engine action named an exact recorded session.\nAmbiguous state preserves the complete private runtime for manual review.'

# shellcheck source=tests/device/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
	cat <<'EOF'
Usage:
  bash tests/device/g2.sh --binary ABSOLUTE_DEBUG_BINARY \
    --archive-v1 ABSOLUTE_OCI_ARCHIVE --archive-v1-sha256 LOWERCASE_SHA256 \
    --archive-v2 ABSOLUTE_OCI_ARCHIVE --archive-v2-sha256 LOWERCASE_SHA256 \
    [--output-root ABSOLUTE_DIR]

Both archives must be reviewed arm64 builds of fixtures/g2/Containerfile,
using G2_FIXTURE_VERSION=v1 and v2 respectively. The supplied binary must be
a debug build: release builds intentionally omit the controlled fault points.
The default acceptance run includes one initial start and at most five retries
using the real 1/2/4/8/16-second restart delays.
EOF
}

binary=
archive_v1=
archive_v1_sha=
archive_v2=
archive_v2_sha=
output_root=
while (($# > 0)); do
	case $1 in
		--binary)
			[[ $# -ge 2 ]] || { device_error "--binary requires a value"; exit 2; }
			binary=$2; shift 2
			;;
		--archive-v1)
			[[ $# -ge 2 ]] || { device_error "--archive-v1 requires a value"; exit 2; }
			archive_v1=$2; shift 2
			;;
		--archive-v1-sha256)
			[[ $# -ge 2 ]] || { device_error "--archive-v1-sha256 requires a value"; exit 2; }
			archive_v1_sha=$2; shift 2
			;;
		--archive-v2)
			[[ $# -ge 2 ]] || { device_error "--archive-v2 requires a value"; exit 2; }
			archive_v2=$2; shift 2
			;;
		--archive-v2-sha256)
			[[ $# -ge 2 ]] || { device_error "--archive-v2-sha256 requires a value"; exit 2; }
			archive_v2_sha=$2; shift 2
			;;
		--output-root)
			[[ $# -ge 2 ]] || { device_error "--output-root requires a value"; exit 2; }
			output_root=$2; shift 2
			;;
		-h | --help) usage; exit 0 ;;
		*) device_error "unknown or incomplete argument: $1"; usage >&2; exit 2 ;;
	esac
done

if [[ -z $binary || -z $archive_v1 || -z $archive_v1_sha || \
	-z $archive_v2 || -z $archive_v2_sha ]]; then
	usage >&2
	exit 2
fi

g2_app_files_root=
g2_canonical_prefix=
g2_canonical_home=
g2_canonical_output_root=
g2_canonical_runtime_root=

g2_is_within_app_files() {
	local path=$1
	[[ -n $g2_app_files_root ]] || return 1
	case $path/ in
		"$g2_app_files_root"/*) return 0 ;;
		*) return 1 ;;
	esac
}

g2_prepare_app_private_roots() {
	local requested_output=$1 prefix=${PREFIX:-} home=${HOME:-}
	local effective_output runtime_root

	[[ $prefix == /* && -d $prefix && ! -L $prefix ]] || {
		device_error "PREFIX must be an absolute real Termux directory"
		return 2
	}
	[[ $home == /* && -d $home && ! -L $home ]] || {
		device_error "HOME must be an absolute real Termux directory"
		return 2
	}
	g2_canonical_prefix=$(cd -- "$prefix" 2>/dev/null && pwd -P) || return 2
	g2_app_files_root=$(cd -- "$g2_canonical_prefix/.." 2>/dev/null && pwd -P) || return 2
	g2_canonical_home=$(cd -- "$home" 2>/dev/null && pwd -P) || return 2
	if [[ ${g2_canonical_prefix##*/} != usr || ${g2_app_files_root##*/} != files || \
		$g2_canonical_home != "$g2_app_files_root/home" ]]; then
		device_error "PREFIX and HOME do not resolve to one canonical Termux app-private files tree"
		return 2
	fi

	effective_output=$requested_output
	[[ -n $effective_output ]] || effective_output=${TMPDIR:-}
	[[ $effective_output == /* && -d $effective_output && ! -L $effective_output ]] || {
		device_error "output root must be an absolute real app-private directory"
		return 2
	}
	g2_canonical_output_root=$(cd -- "$effective_output" 2>/dev/null && pwd -P) || return 2
	runtime_root=${TMPDIR:-$g2_canonical_output_root}
	[[ $runtime_root == /* && -d $runtime_root && ! -L $runtime_root ]] || {
		device_error "TMPDIR must be an absolute real app-private directory"
		return 2
	}
	g2_canonical_runtime_root=$(cd -- "$runtime_root" 2>/dev/null && pwd -P) || return 2
	if ! g2_is_within_app_files "$g2_canonical_output_root" || \
		! g2_is_within_app_files "$g2_canonical_runtime_root"; then
		device_error "output root and TMPDIR must remain below $g2_app_files_root"
		return 2
	fi
}

g2_prepare_app_private_roots "$output_root" || exit $?
device_init "$output_root" || exit $?

G2_CASES_DIR=$DEVICE_EVIDENCE_DIR/cases
G2_MATRIX_FILE=$DEVICE_EVIDENCE_DIR/matrix.tsv
G2_INTENT_FILE=$DEVICE_EVIDENCE_DIR/intent.tsv
G2_REAL_BEFORE=$DEVICE_EVIDENCE_DIR/real-containers.before
G2_REAL_AFTER=$DEVICE_EVIDENCE_DIR/real-containers.after
G2_ARCHIVE_V1_REPORT=$DEVICE_EVIDENCE_DIR/archive-v1.tsv
G2_ARCHIVE_V2_REPORT=$DEVICE_EVIDENCE_DIR/archive-v2.tsv
mkdir -m 0700 -- "$G2_CASES_DIR"
printf 'case\texpectation\tobserved\tcleanup\n' >"$G2_MATRIX_FILE"
printf 'time_utc\taction\ttarget\n' >"$G2_INTENT_FILE"

G2_REAL_PREFIX=$g2_canonical_prefix
G2_REAL_HOME=$g2_canonical_home
G2_REAL_PATH=${PATH:-}
G2_APP_FILES_ROOT=$g2_app_files_root
G2_REPO_ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd -P)
G2_SOURCE_COMMIT=
G2_SOURCE_STATUS=
G2_MANIFEST_V1_DIGEST=
G2_MANIFEST_V2_DIGEST=
G2_ARCHIVE_V1=$DEVICE_RUNTIME_DIR/g2-v1.oci.tar
G2_ARCHIVE_V2=$DEVICE_RUNTIME_DIR/g2-v2.oci.tar
G2_BOOT_ID=
G2_PREFLIGHT_DONE=0
G2_PRESERVE_RUNTIME=0
G2_CLEANUP_FAILURES=0
G2_CLEANUP_STATE=pending
G2_DEFERRED_SIGNAL=0
G2_ACTIVE_DAEMON_PID=
G2_ACTIVE_DAEMON_STARTTIME=
G2_ACTIVE_DAEMON_BOOT_ID=
G2_LAST_CLI_PID=
G2_ACTIVE_CLI_PID=
G2_ACTIVE_CLI_STARTTIME=
G2_ACTIVE_CLI_BOOT_ID=
G2_CASE_SEQUENCE=0
G2_DAEMON_SEQUENCE=0
G2_CASE=
G2_CASE_ROOT=
G2_PREFIX=
G2_HOME=
G2_PROJECT=
G2_FAULT=
G2_RAW=
G2_RUN_ID=$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')

declare -a G2_PREFIXES=()
declare -a G2_HOMES=()

g2_defer_signal() {
	local code=$1
	if ((G2_DEFERRED_SIGNAL == 0)); then G2_DEFERRED_SIGNAL=$code; fi
}

g2_install_deferred_signal_handlers() {
	trap 'g2_defer_signal 129' HUP
	trap 'g2_defer_signal 130' INT
	trap 'g2_defer_signal 143' TERM
}

g2_intent() {
	local action=$1 target=$2
	printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
		"$(device_sanitize_tsv "$action")" "$(device_sanitize_tsv "$target")" \
		>>"$G2_INTENT_FILE" || return 1
	sync -f "$G2_INTENT_FILE"
}

g2_proc_starttime() {
	local pid=$1 line rest
	local -a fields
	[[ $pid =~ ^[1-9][0-9]*$ && -r /proc/$pid/stat ]] || return 1
	IFS= read -r line <"/proc/$pid/stat" || return 1
	[[ $line == *') '* ]] || return 1
	rest=${line##*) }
	read -r -a fields <<<"$rest"
	((${#fields[@]} >= 20)) || return 1
	[[ ${fields[0]} != Z && ${fields[0]} != X && ${fields[0]} != x ]] || return 1
	printf '%s\n' "${fields[19]}"
}

g2_proc_state() {
	local pid=$1 line rest
	local -a fields
	[[ $pid =~ ^[1-9][0-9]*$ && -r /proc/$pid/stat ]] || return 1
	IFS= read -r line <"/proc/$pid/stat" || return 1
	[[ $line == *') '* ]] || return 1
	rest=${line##*) }
	read -r -a fields <<<"$rest"
	((${#fields[@]} >= 20)) || return 1
	printf '%s\n' "${fields[0]}"
}

g2_daemon_identity_matches() {
	local current_starttime current_boot
	[[ $G2_ACTIVE_DAEMON_PID =~ ^[1-9][0-9]*$ ]] || return 1
	current_boot=$(< /proc/sys/kernel/random/boot_id) || return 1
	[[ $current_boot == "$G2_ACTIVE_DAEMON_BOOT_ID" ]] || return 1
	current_starttime=$(g2_proc_starttime "$G2_ACTIVE_DAEMON_PID") || return 1
	[[ $current_starttime == "$G2_ACTIVE_DAEMON_STARTTIME" ]]
}

g2_cli_identity_matches() {
	local current_starttime current_boot
	[[ $G2_ACTIVE_CLI_PID =~ ^[1-9][0-9]*$ ]] || return 1
	current_boot=$(< /proc/sys/kernel/random/boot_id) || return 1
	[[ $current_boot == "$G2_ACTIVE_CLI_BOOT_ID" ]] || return 1
	current_starttime=$(g2_proc_starttime "$G2_ACTIVE_CLI_PID") || return 1
	[[ $current_starttime == "$G2_ACTIVE_CLI_STARTTIME" ]]
}

g2_env() {
	env PREFIX="$G2_PREFIX" TERMUX__PREFIX="$G2_PREFIX" TERMUX__HOME="$G2_HOME" \
		PD_PROOT_BIN="$G2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		COLUMNS=240 PATH="$G2_REAL_PREFIX/bin:$G2_REAL_PATH" "$@"
}

g2_pd() {
	g2_env proot-distro "$@"
}

g2_pd_for() {
	local prefix=$1 home=$2
	shift 2
	env TERMUX__PREFIX="$prefix" TERMUX__HOME="$home" \
		PD_PROOT_BIN="$G2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		COLUMNS=240 PATH="$G2_REAL_PREFIX/bin:$G2_REAL_PATH" proot-distro "$@"
}

g2_wait_pid() {
	local pid=$1 limit=${2:-200} iteration
	for ((iteration = 0; iteration < limit; iteration += 1)); do
		if ! kill -0 "$pid" 2>/dev/null; then
			wait "$pid" 2>/dev/null
			return $?
		fi
		sleep 0.05
	done
	return 124
}

g2_wait_file() {
	local path=$1 iteration
	for ((iteration = 0; iteration < 600; iteration += 1)); do
		[[ -f $path && ! -L $path ]] && return 0
		if [[ -n $G2_ACTIVE_DAEMON_PID ]] && ! kill -0 "$G2_ACTIVE_DAEMON_PID" 2>/dev/null; then
			return 1
		fi
		sleep 0.05
	done
	return 1
}

g2_new_case() {
	local label=$1 initial_list
	[[ -z $G2_ACTIVE_DAEMON_PID && -z $G2_ACTIVE_CLI_PID ]] || return 1
	G2_CASE_SEQUENCE=$((G2_CASE_SEQUENCE + 1))
	G2_CASE=$label
	G2_CASE_ROOT=$DEVICE_RUNTIME_DIR/c$G2_CASE_SEQUENCE
	G2_PREFIX=$G2_CASE_ROOT/p
	G2_HOME=$G2_CASE_ROOT/h
	G2_PROJECT=$G2_CASE_ROOT/project
	G2_FAULT=$G2_CASE_ROOT/fault
	G2_RAW=$G2_CASES_DIR/$label
	G2_DAEMON_SEQUENCE=0
	mkdir -m 0700 -- "$G2_CASE_ROOT" "$G2_PREFIX" "$G2_HOME" \
		"$G2_PROJECT" "$G2_FAULT" "$G2_RAW" || return 1
	G2_PREFIXES+=("$G2_PREFIX")
	G2_HOMES+=("$G2_HOME")
	initial_list=$G2_RAW/engine.initial
	g2_pd list --quiet >"$initial_list" 2>"$initial_list.stderr" || return 1
	[[ ! -s $initial_list ]]
}

g2_start_daemon() {
	local mode=${1:-normal} stdout stderr pid iteration starttime=
	G2_DAEMON_SEQUENCE=$((G2_DAEMON_SEQUENCE + 1))
	stdout=$G2_RAW/daemon-$G2_DAEMON_SEQUENCE.stdout
	stderr=$G2_RAW/daemon-$G2_DAEMON_SEQUENCE.stderr
	if [[ $mode == fault ]]; then
		env -u TERMUX_STACKS_SQLITE_MAX_PAGES -u TERMUX_STACKS_TEST_IMMEDIATE_RESTART \
			PREFIX="$G2_PREFIX" TERMUX__PREFIX="$G2_PREFIX" TERMUX__HOME="$G2_HOME" \
			PD_PROOT_BIN="$G2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
			COLUMNS=240 PATH="$G2_REAL_PREFIX/bin:$G2_REAL_PATH" \
			TERMUX_STACKS_FAULT_DIR="$G2_FAULT" "$binary" daemon \
			>"$stdout" 2>"$stderr" &
	else
		env -u TERMUX_STACKS_FAULT_DIR -u TERMUX_STACKS_SQLITE_MAX_PAGES \
			-u TERMUX_STACKS_TEST_IMMEDIATE_RESTART \
			PREFIX="$G2_PREFIX" TERMUX__PREFIX="$G2_PREFIX" TERMUX__HOME="$G2_HOME" \
			PD_PROOT_BIN="$G2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
			COLUMNS=240 PATH="$G2_REAL_PREFIX/bin:$G2_REAL_PATH" "$binary" daemon \
			>"$stdout" 2>"$stderr" &
	fi
	pid=$!
	G2_ACTIVE_DAEMON_PID=$pid
	G2_ACTIVE_DAEMON_BOOT_ID=$G2_BOOT_ID
	for ((iteration = 0; iteration < 50; iteration += 1)); do
		starttime=$(g2_proc_starttime "$pid") && break
		kill -0 "$pid" 2>/dev/null || return 1
		sleep 0.02
	done
	[[ -n $starttime ]] || return 1
	G2_ACTIVE_DAEMON_STARTTIME=$starttime
	device_wait_for_socket "$pid" "$G2_PREFIX/var/run/termux-stacks/daemon.sock" || return 1
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		grep -F ' daemon ready ' "$stdout" >/dev/null 2>&1 && break
		kill -0 "$pid" 2>/dev/null || return 1
		sleep 0.02
	done
	grep -F ' daemon ready ' "$stdout" >/dev/null 2>&1 || return 1
	g2_daemon_identity_matches
}

g2_stop_daemon() {
	local signal=${1:-TERM} pid=$G2_ACTIVE_DAEMON_PID rc
	[[ -n $pid ]] || return 0
	g2_daemon_identity_matches || return 1
	g2_intent "signal-daemon-$signal" "$pid" || return 1
	kill -"$signal" "$pid" || return 1
	g2_wait_pid "$pid" 240
	rc=$?
	if [[ $signal == KILL ]]; then
		[[ $rc -eq 137 || $rc -eq 0 ]] || return 1
	else
		[[ $rc -eq 0 ]] || return 1
	fi
	G2_ACTIVE_DAEMON_PID=
	G2_ACTIVE_DAEMON_STARTTIME=
	G2_ACTIVE_DAEMON_BOOT_ID=
}

g2_cli() {
	local label=$1
	shift
	g2_env "$binary" "$@" >"$G2_RAW/$label.stdout" 2>"$G2_RAW/$label.stderr"
}

g2_cli_background() {
	local label=$1 pid starttime iteration cleanup_rc
	shift
	g2_intent launch-cli "$label" || return 1
	g2_env "$binary" "$@" >"$G2_RAW/$label.stdout" 2>"$G2_RAW/$label.stderr" &
	pid=$!
	G2_LAST_CLI_PID=$pid
	G2_ACTIVE_CLI_PID=$pid
	G2_ACTIVE_CLI_BOOT_ID=$G2_BOOT_ID
	G2_ACTIVE_CLI_STARTTIME=
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		if starttime=$(g2_proc_starttime "$pid" 2>/dev/null); then
			G2_ACTIVE_CLI_STARTTIME=$starttime
			return 0
		fi
		if ! kill -0 "$pid" 2>/dev/null; then
			wait "$pid" 2>/dev/null || true
			G2_ACTIVE_CLI_PID=
			G2_ACTIVE_CLI_BOOT_ID=
			return 1
		fi
		sleep 0.01
	done
	# `$pid` is still our unreaped direct child, so it cannot have been reused.
	kill -TERM "$pid" 2>/dev/null || true
	g2_wait_pid "$pid" 100 >/dev/null 2>&1
	cleanup_rc=$?
	if [[ $cleanup_rc -eq 124 ]]; then
		kill -KILL "$pid" 2>/dev/null || true
		g2_wait_pid "$pid" 100 >/dev/null 2>&1
		cleanup_rc=$?
	fi
	if [[ $cleanup_rc -ne 124 ]]; then
		G2_ACTIVE_CLI_PID=
		G2_ACTIVE_CLI_BOOT_ID=
		G2_LAST_CLI_PID=
	fi
	return 1
}

g2_wait_interrupted_cli() {
	local expected_pid=$1 label=$2 rc
	[[ $G2_ACTIVE_CLI_PID == "$expected_pid" ]] || return 1
	g2_wait_pid "$expected_pid" 200
	rc=$?
	printf '%s\n' "$rc" >"$G2_RAW/$label.exit"
	if ((rc == 124)); then
		return 1
	fi
	G2_ACTIVE_CLI_PID=
	G2_ACTIVE_CLI_STARTTIME=
	G2_ACTIVE_CLI_BOOT_ID=
	G2_LAST_CLI_PID=
	[[ $rc -eq 1 ]]
}

g2_db_rows() {
	local output=$1 sql=$2 database=$G2_PREFIX/var/lib/termux-stacks/state.db
	python3 - "$database" "$sql" >"$output" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
for row in connection.execute(sys.argv[2]):
    print("\t".join("NULL" if value is None else str(value) for value in row))
PY
}

g2_db_value() {
	local sql=$1 output=$G2_RAW/.db-value
	g2_db_rows "$output" "$sql" || return 1
	[[ $(wc -l <"$output") -eq 1 ]] || return 1
	cat "$output"
}

g2_capture_runtime_evidence() {
	local label=$1 inventory seeds database identity_file rc=0
	[[ $label =~ ^[a-z0-9-]+$ && -n $G2_RAW && -d $G2_RAW ]] || return 1
	inventory=$G2_RAW/runtime-$label.engine-sessions
	seeds=$G2_RAW/runtime-$label.process-seeds
	database=$G2_PREFIX/var/lib/termux-stacks/state.db
	identity_file=$G2_RAW/runtime-$label.daemon-identity.tsv
	: >"$seeds"
	printf 'field\tvalue\nactive_pid\t%s\nrecorded_starttime\t%s\nrecorded_boot_id\t%s\n' \
		"${G2_ACTIVE_DAEMON_PID:-}" "${G2_ACTIVE_DAEMON_STARTTIME:-}" \
		"${G2_ACTIVE_DAEMON_BOOT_ID:-}" >"$identity_file" || rc=1
	if [[ ${G2_ACTIVE_DAEMON_PID:-} =~ ^[1-9][0-9]*$ ]]; then
		printf '%s\n' "$G2_ACTIVE_DAEMON_PID" >>"$seeds"
	fi

	if [[ -n $G2_PREFIX && -d $G2_PREFIX ]]; then
		if g2_pd ps --quiet >"$inventory" 2>"$inventory.stderr"; then
			printf '%s\n' ok >"$inventory.status"
		else
			printf '%s\n' failed >"$inventory.status"
			rc=1
		fi
		awk 'NF == 1 && $1 ~ /^[1-9][0-9]*$/ { print $1 }' \
			"$inventory" >>"$seeds" 2>/dev/null || rc=1
	fi

	if [[ -f $database && ! -L $database ]]; then
		python3 - "$database" >>"$seeds" \
			2>"$G2_RAW/runtime-$label.database-pids.stderr" <<'PY' || rc=1
import sqlite3
import sys

connection = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True, timeout=1)
for row in connection.execute(
    "select session_id from services where session_id is not null "
    "union select child_pid from services where child_pid is not null"
):
    value = row[0]
    if isinstance(value, int) and value > 0:
        print(value)
PY
	fi
	sort -nu -o "$seeds" -- "$seeds" || rc=1

	python3 - "$seeds" >"$G2_RAW/runtime-$label.process-tree.jsonl" \
		2>"$G2_RAW/runtime-$label.process-tree.stderr" <<'PY' || rc=1
import json
import os
import re
import sys

safe_executables = {
    "bash", "busybox", "g2-worker", "httpd", "proot", "proot-distro",
    "sh", "sleep", "termux-stacks",
}
safe_arguments = {
    "daemon", "fail", "httpd", "login", "recover-once", "seed",
    "steady", "web",
}
safe_option = re.compile(r"^-{1,2}[A-Za-z][A-Za-z-]*$")


def redact_argument(raw, index):
    text = raw.decode("utf-8", "replace")
    if index == 0:
        name = os.path.basename(text)
        return name if name in safe_executables else "<redacted-executable>"
    if text in safe_arguments or safe_option.fullmatch(text):
        return text
    return f"<redacted:{len(raw)} bytes>"


def read_process(pid):
    proc = f"/proc/{pid}"
    try:
        stat = open(f"{proc}/stat", "rb").read()
        marker = stat.rfind(b") ")
        if marker < 0:
            raise ValueError("malformed stat")
        comm = stat[stat.find(b"(") + 1:marker].decode("utf-8", "replace")
        fields = stat[marker + 2:].split()
        state = fields[0].decode("ascii", "replace")
        ppid = int(fields[1])
        starttime = int(fields[19])
        try:
            children = [
                int(value)
                for value in open(f"{proc}/task/{pid}/children", "rt").read().split()
                if value.isdecimal() and int(value) > 0
            ]
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            children = []
        try:
            raw_argv = open(f"{proc}/cmdline", "rb").read().split(b"\0")
            raw_argv = [value for value in raw_argv if value]
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            raw_argv = []
        safe_comm = comm if comm in safe_executables else "<redacted>"
        return {
            "pid": pid,
            "ppid": ppid,
            "state": state,
            "starttime": starttime,
            "comm": safe_comm,
            "argv": [redact_argument(value, index) for index, value in enumerate(raw_argv)],
            "children": sorted(set(children)),
        }
    except (FileNotFoundError, PermissionError, ProcessLookupError, ValueError, IndexError) as error:
        return {"pid": pid, "unavailable": type(error).__name__}


with open(sys.argv[1], "rt", encoding="ascii") as seed_file:
    pending = [int(line) for line in seed_file if line.strip().isdecimal()]
seen = set()
records = []
while pending:
    pid = pending.pop(0)
    if pid in seen:
        continue
    seen.add(pid)
    record = read_process(pid)
    records.append(record)
    pending.extend(record.get("children", []))
for record in sorted(records, key=lambda value: value["pid"]):
    print(json.dumps(record, sort_keys=True, separators=(",", ":")))
PY
	return "$rc"
}

g2_dump_case() {
	local database=$G2_PREFIX/var/lib/termux-stacks/state.db log relative destination
	[[ -f $database && ! -L $database ]] || return 0
	python3 - "$database" >"$G2_RAW/database.txt" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
print("integrity_check\t" + connection.execute("pragma integrity_check").fetchone()[0])
tables = {row[0] for row in connection.execute("select name from sqlite_master where type = 'table'")}
for table in ("meta", "stacks", "services", "rootfs_generations", "operations", "operation_services"):
    if table not in tables:
        continue
    print(f"[{table}]")
    for row in connection.execute(f"select * from {table} order by rowid"):
        print("\t".join("NULL" if value is None else str(value) for value in row))
PY
	while IFS= read -r log; do
		relative=${log#"$G2_PREFIX/var/lib/termux-stacks/logs/"}
		destination=${relative//\//-}
		cp -- "$log" "$G2_RAW/$destination" || return 1
	done < <(find "$G2_PREFIX/var/lib/termux-stacks/logs" -type f 2>/dev/null | sort)
}

g2_two_ports() {
	python3 - <<'PY'
import socket

sockets = []
for _ in range(2):
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    sockets.append(sock)
print(sockets[0].getsockname()[1], sockets[1].getsockname()[1])
PY
}

g2_write_stack_manifest() {
	local stack=$1 archive=$2 port=$3 token=$4 path=$5
	local input=$G2_PROJECT/input-$stack events=$G2_PROJECT/events-$stack
	mkdir -p -m 0700 -- "$input" "$events" || return 1
	[[ -d $input && ! -L $input && -d $events && ! -L $events ]] || return 1
	printf '%s\n' "$token" >"$input/token" || return 1
	chmod 0600 "$input/token" || return 1
	cat >"$path" <<EOF
apiVersion: termux-stacks/v1alpha1
kind: Stack
metadata:
  name: $stack
volumes:
  data: {}
services:
  seed:
    image: '$archive'
    command: [steady, seed]
    environment:
      G2_BIND_TOKEN: '$token'
      G2_LITERAL: 'literal \$HOME * ; [g2]'
    mounts:
      - {type: bind, source: ./input-$stack, target: /input}
      - {type: bind, source: ./events-$stack, target: /events}
      - {type: volume, source: data, target: /state}
  web:
    image: '$archive'
    command: [steady, web]
    dependsOn: [seed]
    environment:
      G2_BIND_TOKEN: '$token'
      G2_LITERAL: 'literal \$HOME * ; [g2]'
      G2_PORT: '$port'
    mounts:
      - {type: bind, source: ./input-$stack, target: /input}
      - {type: bind, source: ./events-$stack, target: /events}
      - {type: volume, source: data, target: /state}
    ports:
      - {address: 127.0.0.1, port: $port}
EOF
	chmod 0600 "$path"
}

g2_write_restart_manifest() {
	local stack=$1 mode=$2 policy=$3 path=$4 events=$G2_PROJECT/events-$stack
	mkdir -p -m 0700 -- "$events" || return 1
	[[ -d $events && ! -L $events ]] || return 1
	cat >"$path" <<EOF
apiVersion: termux-stacks/v1alpha1
kind: Stack
metadata:
  name: $stack
services:
  app:
    image: '$G2_ARCHIVE_V1'
    command: [$mode]
    mounts:
      - {type: bind, source: ./events-$stack, target: /events}
    restart: $policy
EOF
	chmod 0600 "$path"
}

g2_wait_http() {
	local port=$1 output=$2 iteration
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		if python3 - "$port" >"$output" 2>"$output.stderr" <<'PY'
import socket
import sys

with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1) as connection:
    connection.sendall(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
    chunks = []
    while True:
        chunk = connection.recv(65536)
        if not chunk:
            break
        chunks.append(chunk)
response = b"".join(chunks)
headers, separator, body = response.partition(b"\r\n\r\n")
status_line = headers.split(b"\r\n", 1)[0]
if not separator or not status_line.startswith(b"HTTP/1.") or b" 200 " not in status_line:
    raise SystemExit(1)
sys.stdout.buffer.write(body)
PY
		then return 0; fi
		sleep 0.05
	done
	return 1
}

g2_capture_engine_sessions() {
	local output=$1
	g2_pd ps --quiet >"$output.unsorted" 2>"$output.stderr" || return 1
	awk 'NF != 1 || $1 !~ /^[1-9][0-9]*$/ { exit 1 } { print $1 }' \
		"$output.unsorted" | sort -n >"$output"
}

g2_exact_kill_sessions() {
	local sessions_file=$1 label=$2 session iteration current
	while IFS= read -r session; do
		[[ $session =~ ^[1-9][0-9]*$ ]] || return 1
		current=$G2_RAW/$label-$session.before
		g2_capture_engine_sessions "$current" || return 1
		grep -Fx "$session" "$current" >/dev/null || return 1
		g2_intent engine-kill-exact-session "$session" || return 1
		g2_pd kill "$session" >"$G2_RAW/$label-$session.kill.stdout" \
			2>"$G2_RAW/$label-$session.kill.stderr" || return 1
		for ((iteration = 0; iteration < 200; iteration += 1)); do
			g2_capture_engine_sessions "$G2_RAW/$label-$session.after" || return 1
			grep -Fx "$session" "$G2_RAW/$label-$session.after" >/dev/null || break
			sleep 0.05
		done
		grep -Fx "$session" "$G2_RAW/$label-$session.after" >/dev/null && return 1
	done <"$sessions_file"
	g2_capture_engine_sessions "$G2_RAW/$label-final" || return 1
	[[ ! -s $G2_RAW/$label-final ]]
}

g2_allow_before() {
	local target=$1 checkpoint
	for checkpoint in before_intent after_intent after_install after_start \
		between_service_starts before_commit during_down during_backoff; do
		[[ $checkpoint == "$target" ]] && return 0
		: >"$G2_FAULT/$checkpoint.continue" || return 1
	done
	return 1
}

g2_fail_case() {
	local result_id=$1 detail=$2
	device_result "$result_id" FAIL 1 "$detail; private runtime preserved" - -
	G2_PRESERVE_RUNTIME=1
	G2_CLEANUP_FAILURES=$((G2_CLEANUP_FAILURES + 1))
	g2_capture_runtime_evidence failure || true
	g2_dump_case || true
	return 1
}

g2_require_case() {
	local label=$1 function_name=$2 failures_before=$G2_CLEANUP_FAILURES
	if "$function_name"; then
		return 0
	fi
	G2_PRESERVE_RUNTIME=1
	if ((G2_CLEANUP_FAILURES == failures_before)); then
		G2_CLEANUP_FAILURES=$((G2_CLEANUP_FAILURES + 1))
		device_result "g2.internal.$label" FAIL 1 \
			"case aborted before its acceptance assertion; private runtime preserved" - -
	fi
	return 1
}

g2_normal_case() {
	local alpha=g2-alpha beta=g2-beta conflict=g2-conflict
	local alpha_manifest beta_manifest conflict_manifest alpha_port beta_port
	local alpha_token=alpha-$G2_RUN_ID beta_token=beta-$G2_RUN_ID
	local alpha_events beta_events expected body
	local alpha_alias_before alpha_alias_after alpha_generation_before alpha_generation_after
	local beta_alias_before beta_alias_after beta_generation_before beta_generation_after

	g2_new_case normal || { g2_fail_case g2.lifecycle.concurrent "cannot create isolated normal case"; return 1; }
	if ! g2_start_daemon normal; then
		g2_fail_case g2.lifecycle.concurrent "cannot start the isolated normal-case daemon"
		return 1
	fi
	# Port ownership cannot be leased across a PRoot start. Allocate as late as
	# possible so the documented best-effort race is limited to manifest write/up.
	read -r alpha_port beta_port < <(g2_two_ports) || return 1
	[[ $alpha_port != "$beta_port" ]] || return 1
	alpha_manifest=$G2_PROJECT/alpha.yaml
	beta_manifest=$G2_PROJECT/beta.yaml
	conflict_manifest=$G2_PROJECT/conflict.yaml
	alpha_events=$G2_PROJECT/events-$alpha
	beta_events=$G2_PROJECT/events-$beta
	g2_write_stack_manifest "$alpha" "$G2_ARCHIVE_V1" "$alpha_port" "$alpha_token" "$alpha_manifest" || return 1
	g2_write_stack_manifest "$beta" "$G2_ARCHIVE_V1" "$beta_port" "$beta_token" "$beta_manifest" || return 1
	if ! g2_cli up-alpha up "$alpha_manifest" || \
		! g2_cli up-beta up "$beta_manifest" || ! g2_cli status-alpha status "$alpha" || \
		! g2_cli status-beta status "$beta" || \
		! jq -e '.observed_state == "running" and .revision == 1 and
			(.services | length == 2) and all(.services[]; .observed_state == "running" and .rootfs_state == "installed")' \
			"$G2_RAW/status-alpha.stdout" >/dev/null || \
		! jq -e '.observed_state == "running" and .revision == 1 and
			(.services | length == 2) and all(.services[]; .observed_state == "running" and .rootfs_state == "installed")' \
			"$G2_RAW/status-beta.stdout" >/dev/null || \
		! g2_wait_file "$alpha_events/web.ready" || ! g2_wait_file "$beta_events/web.ready" || \
		! g2_wait_http "$alpha_port" "$G2_RAW/alpha.http" || \
		! g2_wait_http "$beta_port" "$G2_RAW/beta.http"; then
		g2_fail_case g2.lifecycle.concurrent "two simultaneous two-service stacks did not become running"
		return 1
	fi
	printf 'role=web version=v1 bind=%s volume=v1\n' "$alpha_token" >"$G2_RAW/alpha.http.expected"
	printf 'role=web version=v1 bind=%s volume=v1\n' "$beta_token" >"$G2_RAW/beta.http.expected"
	g2_db_rows "$G2_RAW/sessions.database" \
		"select session_id from services where stack_name in ('$alpha','$beta') order by session_id" || return 1
	g2_capture_engine_sessions "$G2_RAW/sessions.engine" || return 1
	if ! cmp -s "$G2_RAW/alpha.http.expected" "$G2_RAW/alpha.http" || \
		! cmp -s "$G2_RAW/beta.http.expected" "$G2_RAW/beta.http" || \
		! cmp -s "$G2_RAW/sessions.database" "$G2_RAW/sessions.engine" || \
		[[ $(wc -l <"$G2_RAW/sessions.engine") -ne 4 ]] || \
		! g2_db_rows "$G2_RAW/dag.tsv" \
		"select stack_name,service_name,ordinal from operation_services where stack_name in ('$alpha','$beta') order by stack_name,ordinal" || \
		! printf '%s\n' "$alpha	seed	0" "$alpha	web	1" "$beta	seed	0" "$beta	web	1" \
			| cmp -s - "$G2_RAW/dag.tsv"; then
		g2_fail_case g2.lifecycle.concurrent "session ownership, HTTP endpoints, or stable DAG order did not match"
		return 1
	fi
	device_result g2.lifecycle.concurrent PASS 0 \
		"two simultaneous two-service stacks; stable seed->web DAG; four exact sessions" - -

	g2_db_rows "$G2_RAW/beta-during-alpha-restart.before" \
		"select name,current_alias,current_generation,session_id,child_pid,child_starttime,boot_id from services where stack_name='$beta' order by name" || return 1
	g2_db_rows "$G2_RAW/alpha-seed-during-web-restart.before" \
		"select session_id,child_pid,child_starttime,boot_id from services where stack_name='$alpha' and name='seed'" || return 1
	if ! g2_cli logs-alpha-seed logs "$alpha" seed --tail 20 || \
		! jq -e --arg token "$alpha_token" '
			any(.stdout[]; contains("literal=<literal $HOME * ; [g2]>") ) and
			any(.stdout[]; contains("bind=<" + $token + ">") ) and
			any(.stdout[]; contains("volume=<v1>"))
		' "$G2_RAW/logs-alpha-seed.stdout" >/dev/null; then
		g2_fail_case g2.resources.logs-restart "alpha literal environment, relative bind, volume, or logs tail failed"
		return 1
	fi
	alpha_alias_before=$(g2_db_value "select current_alias from services where stack_name='$alpha' and name='web'") || return 1
	alpha_generation_before=$(g2_db_value "select current_generation from services where stack_name='$alpha' and name='web'") || return 1
	g2_db_rows "$G2_RAW/alpha-web-session.before" \
		"select session_id,child_starttime from services where stack_name='$alpha' and name='web'" || return 1
	if ! g2_cli restart-alpha-web restart "$alpha" web || \
		! g2_cli logs-alpha-web logs "$alpha" web --tail 50; then
		g2_fail_case g2.resources.logs-restart "alpha per-service restart or logs failed"
		return 1
	fi
	alpha_alias_after=$(g2_db_value "select current_alias from services where stack_name='$alpha' and name='web'") || return 1
	alpha_generation_after=$(g2_db_value "select current_generation from services where stack_name='$alpha' and name='web'") || return 1
	g2_db_rows "$G2_RAW/alpha-web-session.after" \
		"select session_id,child_starttime from services where stack_name='$alpha' and name='web'" || return 1
	g2_db_rows "$G2_RAW/alpha-seed-during-web-restart.after" \
		"select session_id,child_pid,child_starttime,boot_id from services where stack_name='$alpha' and name='seed'" || return 1
	g2_db_rows "$G2_RAW/beta-during-alpha-restart.after" \
		"select name,current_alias,current_generation,session_id,child_pid,child_starttime,boot_id from services where stack_name='$beta' order by name" || return 1
	if [[ $alpha_alias_before != "$alpha_alias_after" || \
		$alpha_generation_before != "$alpha_generation_after" ]] || \
		cmp -s "$G2_RAW/alpha-web-session.before" "$G2_RAW/alpha-web-session.after" || \
		! cmp -s "$G2_RAW/alpha-seed-during-web-restart.before" \
			"$G2_RAW/alpha-seed-during-web-restart.after" || \
		! cmp -s "$G2_RAW/beta-during-alpha-restart.before" \
			"$G2_RAW/beta-during-alpha-restart.after" || \
		! jq -e '[.stdout[] | select(contains("start role=web"))] | length >= 2' \
			"$G2_RAW/logs-alpha-web.stdout" >/dev/null || \
		! g2_wait_http "$alpha_port" "$G2_RAW/alpha-after-restart.http" || \
		! cmp -s "$G2_RAW/alpha.http.expected" \
			"$G2_RAW/alpha-after-restart.http"; then
		g2_fail_case g2.resources.logs-restart "alpha restart changed another service/stack or did not replace only its web session"
		return 1
	fi

	g2_db_rows "$G2_RAW/alpha-during-beta-restart.before" \
		"select name,current_alias,current_generation,session_id,child_pid,child_starttime,boot_id from services where stack_name='$alpha' order by name" || return 1
	g2_db_rows "$G2_RAW/beta-seed-during-web-restart.before" \
		"select session_id,child_pid,child_starttime,boot_id from services where stack_name='$beta' and name='seed'" || return 1
	if ! g2_cli logs-beta-seed logs "$beta" seed --tail 20 || \
		! jq -e --arg token "$beta_token" '
			any(.stdout[]; contains("literal=<literal $HOME * ; [g2]>") ) and
			any(.stdout[]; contains("bind=<" + $token + ">") ) and
			any(.stdout[]; contains("volume=<v1>"))
		' "$G2_RAW/logs-beta-seed.stdout" >/dev/null; then
		g2_fail_case g2.resources.logs-restart "beta literal environment, relative bind, volume, or logs tail failed"
		return 1
	fi
	beta_alias_before=$(g2_db_value "select current_alias from services where stack_name='$beta' and name='web'") || return 1
	beta_generation_before=$(g2_db_value "select current_generation from services where stack_name='$beta' and name='web'") || return 1
	g2_db_rows "$G2_RAW/beta-web-session.before" \
		"select session_id,child_starttime from services where stack_name='$beta' and name='web'" || return 1
	if ! g2_cli restart-beta-web restart "$beta" web || \
		! g2_cli logs-beta-web logs "$beta" web --tail 50; then
		g2_fail_case g2.resources.logs-restart "beta per-service restart or logs failed"
		return 1
	fi
	beta_alias_after=$(g2_db_value "select current_alias from services where stack_name='$beta' and name='web'") || return 1
	beta_generation_after=$(g2_db_value "select current_generation from services where stack_name='$beta' and name='web'") || return 1
	g2_db_rows "$G2_RAW/beta-web-session.after" \
		"select session_id,child_starttime from services where stack_name='$beta' and name='web'" || return 1
	g2_db_rows "$G2_RAW/beta-seed-during-web-restart.after" \
		"select session_id,child_pid,child_starttime,boot_id from services where stack_name='$beta' and name='seed'" || return 1
	g2_db_rows "$G2_RAW/alpha-during-beta-restart.after" \
		"select name,current_alias,current_generation,session_id,child_pid,child_starttime,boot_id from services where stack_name='$alpha' order by name" || return 1
	if [[ $beta_alias_before != "$beta_alias_after" || \
		$beta_generation_before != "$beta_generation_after" ]] || \
		cmp -s "$G2_RAW/beta-web-session.before" "$G2_RAW/beta-web-session.after" || \
		! cmp -s "$G2_RAW/beta-seed-during-web-restart.before" \
			"$G2_RAW/beta-seed-during-web-restart.after" || \
		! cmp -s "$G2_RAW/alpha-during-beta-restart.before" \
			"$G2_RAW/alpha-during-beta-restart.after" || \
		! jq -e '[.stdout[] | select(contains("start role=web"))] | length >= 2' \
			"$G2_RAW/logs-beta-web.stdout" >/dev/null || \
		! g2_wait_http "$beta_port" "$G2_RAW/beta-after-restart.http" || \
		! g2_wait_http "$alpha_port" "$G2_RAW/alpha-after-beta-restart.http" || \
		! cmp -s "$G2_RAW/beta.http.expected" "$G2_RAW/beta-after-restart.http" || \
		! cmp -s "$G2_RAW/alpha.http.expected" "$G2_RAW/alpha-after-beta-restart.http"; then
		g2_fail_case g2.resources.logs-restart "beta restart changed another service/stack or did not replace only its web session"
		return 1
	fi
	device_result g2.resources.logs-restart PASS 0 \
		"both stacks completed bounded logs and an exact web restart; the peer service and peer stack identities were byte-identical" - -

	cat >"$conflict_manifest" <<EOF
apiVersion: termux-stacks/v1alpha1
kind: Stack
metadata: {name: $conflict}
services:
  app:
    image: '$G2_ARCHIVE_V1'
    command: [fail]
    ports: [{address: 127.0.0.1, port: $alpha_port}]
EOF
	chmod 0600 "$conflict_manifest"
	if g2_cli port-conflict up "$conflict_manifest" || \
		! grep -F '[conflict]' "$G2_RAW/port-conflict.stderr" >/dev/null || \
		[[ $(g2_db_value "select count(*) from stacks where name='$conflict'") != 0 ]]; then
		g2_fail_case g2.resources.port-conflict "cross-stack loopback-port conflict was not rejected before intent"
		return 1
	fi
	device_result g2.resources.port-conflict PASS 0 \
		"a fixed loopback port was reachable and a duplicate declaration failed before intent" - -

	g2_db_rows "$G2_RAW/alpha-generation.before" \
		"select name,current_alias,current_generation from services where stack_name='$alpha' order by name" || return 1
	g2_db_rows "$G2_RAW/beta-identity.before" \
		"select name,desired_state,observed_state,effect_phase,current_alias,current_generation,session_id,child_pid,child_starttime,boot_id from services where stack_name='$beta' order by name" || return 1
	: >"$alpha_events/stop.order"
	if ! g2_cli down-alpha down "$alpha" || \
		! printf '%s\n' web seed | cmp -s - "$alpha_events/stop.order"; then
		g2_fail_case g2.update.volume-generations "explicit down did not use reverse DAG order"
		return 1
	fi
	rm -f -- "$alpha_events/seed.ready" "$alpha_events/web.ready"
	g2_write_stack_manifest "$alpha" "$G2_ARCHIVE_V2" "$alpha_port" "$alpha_token" "$alpha_manifest" || return 1
	if ! g2_cli up-alpha-v2 up "$alpha_manifest" || ! g2_cli status-alpha-v2 status "$alpha" || \
		! g2_wait_file "$alpha_events/web.ready" || \
		! g2_wait_http "$alpha_port" "$G2_RAW/alpha-v2.http"; then
		g2_fail_case g2.update.volume-generations "v2 candidate did not commit after explicit down"
		return 1
	fi
	g2_db_rows "$G2_RAW/alpha-generation.after" \
		"select name,current_alias,current_generation from services where stack_name='$alpha' order by name" || return 1
	g2_db_rows "$G2_RAW/beta-identity.after" \
		"select name,desired_state,observed_state,effect_phase,current_alias,current_generation,session_id,child_pid,child_starttime,boot_id from services where stack_name='$beta' order by name" || return 1
	g2_db_rows "$G2_RAW/alpha-rootfs.tsv" \
		"select service_name,generation,role,state from rootfs_generations where stack_name='$alpha' order by service_name,generation" || return 1
	expected=$G2_RAW/alpha-rootfs.expected
	printf '%s\n' 'seed	1	retired	installed' 'seed	2	current	installed' \
		'web	1	retired	installed' 'web	2	current	installed' >"$expected"
	printf 'role=web version=v2 bind=%s volume=v1\n' "$alpha_token" >"$G2_RAW/alpha-v2.http.expected"
	body=$G2_PREFIX/var/lib/termux-stacks/volumes/$alpha/data/value
	if ! jq -e '.observed_state == "running" and .revision == 2 and
		all(.services[]; .observed_state == "running" and .generation == 2)' \
		"$G2_RAW/status-alpha-v2.stdout" >/dev/null || \
		! awk 'NR==FNR { old[$1]=$2; next }
			!($1 in old) || old[$1] == $2 { failed=1 }
			END { exit (NR == 4 && !failed) ? 0 : 1 }' \
			"$G2_RAW/alpha-generation.before" "$G2_RAW/alpha-generation.after" || \
		! cmp -s "$expected" "$G2_RAW/alpha-rootfs.tsv" || \
		! cmp -s "$G2_RAW/beta-identity.before" "$G2_RAW/beta-identity.after" || \
		! cmp -s "$G2_RAW/alpha-v2.http.expected" "$G2_RAW/alpha-v2.http" || \
		[[ ! -f $body || $(<"$body") != v1 ]]; then
		g2_fail_case g2.update.volume-generations "new aliases, retained generations, volume persistence, or stack independence failed"
		return 1
	fi
	device_result g2.update.volume-generations PASS 0 \
		"explicit down/up v1->v2; new aliases; retired generation retained; volume stayed v1; beta unchanged" - -

	: >"$alpha_events/stop.order"
	: >"$beta_events/stop.order"
	if ! g2_cli down-alpha-final down "$alpha" || ! g2_cli down-beta-final down "$beta" || \
		! printf '%s\n' web seed | cmp -s - "$alpha_events/stop.order" || \
		! printf '%s\n' web seed | cmp -s - "$beta_events/stop.order" || \
		! g2_capture_engine_sessions "$G2_RAW/sessions.final" || \
		[[ -s $G2_RAW/sessions.final ]] || ! g2_stop_daemon TERM; then
		g2_fail_case g2.lifecycle.reverse-down "reverse-order final down or exact drain failed"
		return 1
	fi
	device_result g2.lifecycle.reverse-down PASS 0 \
		"both stacks stopped web before seed; all four sessions drained; rootfs and volumes retained" - -
	g2_dump_case
}

g2_restart_cap_case() {
	local stack=g2-restart-cap manifest iteration complete=0 starts failures
	local deadline_samples deadlines sample
	g2_new_case restart-cap || { g2_fail_case g2.restart.cap "cannot create restart-cap case"; return 1; }
	manifest=$G2_PROJECT/cap.yaml
	starts=$G2_PROJECT/events-$stack/start.tsv
	failures=$G2_PROJECT/events-$stack/failure.tsv
	deadline_samples=$G2_RAW/restart-deadlines.samples.tsv
	deadlines=$G2_RAW/restart-deadlines.tsv
	sample=$G2_RAW/restart-deadline.current.tsv
	: >"$deadline_samples"
	g2_write_restart_manifest "$stack" fail on-failure "$manifest" || return 1
	if ! g2_start_daemon normal || ! g2_cli cap-up up "$manifest"; then
		g2_fail_case g2.restart.cap "initial capped-failure service did not qualify"
		return 1
	fi
	for ((iteration = 0; iteration < 500; iteration += 1)); do
		if g2_cli cap-status status "$stack"; then
			g2_db_rows "$sample" \
				"select restart_attempts,next_restart_at from services where stack_name='$stack' and name='app' and effect_phase='backoff' and next_restart_at is not null" || return 1
			if [[ -s $sample ]]; then cat "$sample" >>"$deadline_samples" || return 1; fi
			if jq -e '
			.services[0].restart_attempts == 5 and
			.services[0].observed_state == "failed" and
			.services[0].next_restart_at == null
			' "$G2_RAW/cap-status.stdout" >/dev/null; then
				complete=1
				break
			fi
		fi
		sleep 0.2
	done
	sort -n -k1,1 -k2,2 -u "$deadline_samples" >"$deadlines" || return 1
	if ((complete == 0)) || [[ ! -f $starts || ! -f $failures ]] || \
		! python3 - "$failures" "$starts" "$deadlines" \
			>"$G2_RAW/restart-timing.tsv" 2>"$G2_RAW/restart-timing.stderr" <<'PY'
import sys


def rows(path, expected_columns):
    parsed = []
    with open(path, "rt", encoding="utf-8") as source:
        for number, line in enumerate(source, 1):
            fields = line.rstrip("\n").split("\t")
            if len(fields) != expected_columns:
                raise ValueError(f"{path}:{number}: expected {expected_columns} columns")
            parsed.append(fields)
    return parsed


failures = rows(sys.argv[1], 4)
starts = rows(sys.argv[2], 4)
deadline_rows = rows(sys.argv[3], 2)
if len(failures) != 6 or len(starts) != 6 or len(deadline_rows) != 5:
    raise SystemExit("expected six starts/failures and five durable deadlines")

deadlines = {}
for fields in deadline_rows:
    attempt, deadline = map(int, fields)
    if attempt in deadlines:
        raise SystemExit(f"restart attempt {attempt} had more than one durable deadline")
    deadlines[attempt] = deadline
if set(deadlines) != set(range(1, 6)):
    raise SystemExit("durable restart attempts were not exactly 1..5")

delays = [1, 2, 4, 8, 16]
print("attempt\tcandidate_delay\tfailure_at\tdurable_deadline\tnext_start_at\texit_to_start")
for attempt, delay in enumerate(delays, 1):
    failure = failures[attempt - 1]
    current_start = starts[attempt - 1]
    next_start = starts[attempt]
    if failure[:2] != current_start[:2] or failure[3] != current_start[3]:
        raise SystemExit(f"failure marker {attempt} does not belong to its started process")
    failure_at = int(failure[2])
    deadline = deadlines[attempt]
    next_start_at = int(next_start[2])
    if deadline - failure_at < delay:
        raise SystemExit(f"attempt {attempt} durable deadline was shorter than {delay}s")
    if next_start_at < deadline or next_start_at - failure_at < delay:
        raise SystemExit(f"attempt {attempt} restarted before its durable minimum")
    print(attempt, delay, failure_at, deadline, next_start_at,
          next_start_at - failure_at, sep="\t")

final_failure = failures[-1]
final_start = starts[-1]
if final_failure[:2] != final_start[:2] or final_failure[3] != final_start[3]:
    raise SystemExit("final failure marker does not belong to the final retry")
PY
	then
		g2_fail_case g2.restart.cap "one initial start and the at-most-five-retry cap, durable deadlines, or exit-to-next-start delays were not observed"
		return 1
	fi
	if ! g2_capture_engine_sessions "$G2_RAW/cap-sessions.final" || \
		[[ -s $G2_RAW/cap-sessions.final ]] || \
		! g2_cli cap-logs logs "$stack" app --tail 20 || \
		! jq -e '
			(.stdout | type == "array") and (.stderr | type == "array") and
			([.stdout[] | select(contains("start role=fail"))] | length == 6) and
			([.stderr[] | select(contains("planned capped failure"))] | length == 6)
		' "$G2_RAW/cap-logs.stdout" >/dev/null || ! g2_stop_daemon TERM; then
		g2_fail_case g2.restart.cap "capped service logs were not preserved as separate bounded streams or the service did not drain cleanly"
		return 1
	fi
	device_result g2.restart.cap PASS 0 \
		"on-failure used one initial start and at most five retries; every durable and observed delay met 1/2/4/8/16 seconds" - -
	g2_dump_case
}

g2_fault_between_case() {
	local stack=g2-fault-between manifest port unused session_file cli_pid
	g2_new_case fault-between || { g2_fail_case g2.fault.between-service-starts "cannot create case"; return 1; }
	read -r port unused < <(g2_two_ports) || return 1
	manifest=$G2_PROJECT/stack.yaml
	g2_write_stack_manifest "$stack" "$G2_ARCHIVE_V1" "$port" "between-$G2_RUN_ID" "$manifest" || return 1
	g2_allow_before between_service_starts || return 1
	if ! g2_start_daemon fault; then g2_fail_case g2.fault.between-service-starts "fault daemon did not start"; return 1; fi
	if ! g2_cli_background interrupted-up up "$manifest"; then
		g2_fail_case g2.fault.between-service-starts "fault request client could not be qualified"
		return 1
	fi
	cli_pid=$G2_LAST_CLI_PID
	if ! g2_wait_file "$G2_FAULT/between_service_starts.reached"; then
		g2_fail_case g2.fault.between-service-starts "between-service checkpoint was not reached"
		return 1
	fi
	session_file=$G2_RAW/session.before
	g2_db_rows "$session_file" "select session_id from services where stack_name='$stack' and name='seed'" || return 1
	g2_capture_engine_sessions "$G2_RAW/engine.before" || return 1
	if ! cmp -s "$session_file" "$G2_RAW/engine.before" || ! g2_stop_daemon KILL; then
		g2_fail_case g2.fault.between-service-starts "owned first service could not be qualified before daemon kill"
		return 1
	fi
	if ! g2_wait_interrupted_cli "$cli_pid" interrupted-up; then
		g2_fail_case g2.fault.between-service-starts "fault request client did not terminate with the expected failure"
		return 1
	fi
	if ! g2_start_daemon normal || ! g2_cli recovered status "$stack" || \
		! jq -e '
			.observed_state == "unknown" and
			(any(.services[]; .name == "seed" and .observed_state == "unknown" and .rootfs_state == "installed")) and
			(any(.services[]; .name == "web" and .observed_state == "failed" and .rootfs_state == "absent"))
		' "$G2_RAW/recovered.stdout" >/dev/null || \
		! g2_capture_engine_sessions "$G2_RAW/engine.after" || \
		! cmp -s "$session_file" "$G2_RAW/engine.after"; then
		g2_fail_case g2.fault.between-service-starts "cold recovery retried an effect or did not fail closed"
		return 1
	fi
	sleep 1
	if ! g2_capture_engine_sessions "$G2_RAW/engine.stable" || \
		! cmp -s "$session_file" "$G2_RAW/engine.stable" || ! g2_stop_daemon TERM || \
		! g2_exact_kill_sessions "$session_file" between-cleanup; then
		g2_fail_case g2.fault.between-service-starts "unknown session changed or exact cleanup failed"
		return 1
	fi
	printf '%s\t%s\t%s\t%s\n' between-service-starts \
		"first running; second untouched" "unknown/installed + failed/absent; one unchanged session" \
		"exact recorded session" >>"$G2_MATRIX_FILE"
	device_result g2.fault.between-service-starts PASS 0 \
		"cold recovery created no dependent service and preserved the one ambiguous session for exact cleanup" - -
	g2_dump_case
}

g2_fault_before_commit_case() {
	local stack=g2-fault-before-commit manifest port unused sessions cleanup_sessions
	local starts cli_pid
	g2_new_case fault-before-commit || {
		g2_fail_case g2.fault.before-commit "cannot create case"
		return 1
	}
	if ! g2_start_daemon fault; then
		g2_fail_case g2.fault.before-commit "fault daemon did not start"
		return 1
	fi
	read -r port unused < <(g2_two_ports) || return 1
	manifest=$G2_PROJECT/stack.yaml
	starts=$G2_PROJECT/events-$stack/start.tsv
	g2_write_stack_manifest "$stack" "$G2_ARCHIVE_V1" "$port" \
		"before-commit-$G2_RUN_ID" "$manifest" || return 1
	g2_allow_before before_commit || return 1
	if ! g2_cli_background interrupted-up up "$manifest"; then
		g2_fail_case g2.fault.before-commit "fault request client could not be qualified"
		return 1
	fi
	cli_pid=$G2_LAST_CLI_PID
	if ! g2_wait_file "$G2_FAULT/before_commit.reached" || \
		! g2_wait_file "$G2_PROJECT/events-$stack/web.ready"; then
		g2_fail_case g2.fault.before-commit "before-commit checkpoint or dependent readiness was not reached"
		return 1
	fi
	sessions=$G2_RAW/sessions.before.sorted
	cleanup_sessions=$G2_RAW/sessions.before.reverse-dag
	g2_db_rows "$cleanup_sessions" \
		"select session_id from services where stack_name='$stack' order by case name when 'web' then 0 else 1 end" || return 1
	sort -n "$cleanup_sessions" >"$sessions" || return 1
	g2_capture_engine_sessions "$G2_RAW/engine.before" || return 1
	cp -- "$starts" "$G2_RAW/starts.before" || return 1
	g2_db_rows "$G2_RAW/parent.before" \
		"select operation,phase,coalesce(outcome,'NULL'),coalesce(error_code,'NULL') from operations where stack_name='$stack' order by rowid" || return 1
	if [[ $(wc -l <"$cleanup_sessions") -ne 2 ]] || \
		! cmp -s "$sessions" "$G2_RAW/engine.before" || \
		! awk -F '\t' 'NR == 1 { ok = ($1 == "seed") } NR == 2 { ok = ok && ($1 == "web") } END { exit (NR == 2 && ok) ? 0 : 1 }' \
			"$G2_RAW/starts.before" || \
		! printf '%s\n' $'up\tintent\tNULL\tNULL' \
			| cmp -s - "$G2_RAW/parent.before" || ! g2_stop_daemon KILL; then
		g2_fail_case g2.fault.before-commit "both owned services or the single parent operation could not be qualified before daemon kill"
		return 1
	fi
	if ! g2_wait_interrupted_cli "$cli_pid" interrupted-up; then
		g2_fail_case g2.fault.before-commit "fault request client did not terminate with the expected failure"
		return 1
	fi
	if ! g2_start_daemon normal || ! g2_cli recovered status "$stack" || \
		! jq -e '
			.revision == 0 and .desired_state == "running" and
			.observed_state == "unknown" and
			(.services | length == 2) and
			all(.services[]; .observed_state == "unknown" and
				.effect_phase == "unknown" and .rootfs_state == "installed" and
				.session_id == null)
		' "$G2_RAW/recovered.stdout" >/dev/null || \
		! g2_db_rows "$G2_RAW/services.after" \
			"select name,desired_state,observed_state,effect_phase,coalesce(session_id,'NULL'),coalesce(child_pid,'NULL'),coalesce(child_starttime,'NULL'),coalesce(boot_id,'NULL') from services where stack_name='$stack' and active=1 order by name" || \
		! printf '%s\n' \
			$'seed\trunning\tunknown\tunknown\tNULL\tNULL\tNULL\tNULL' \
			$'web\trunning\tunknown\tunknown\tNULL\tNULL\tNULL\tNULL' \
			| cmp -s - "$G2_RAW/services.after" || \
		! g2_db_rows "$G2_RAW/parent.after" \
			"select operation,phase,outcome,error_code from operations where stack_name='$stack' order by rowid" || \
		! printf '%s\n' $'up\tunknown\tfailure\tcold_start_unknown' \
			| cmp -s - "$G2_RAW/parent.after" || \
		! g2_db_rows "$G2_RAW/children.after" \
			"select service_name,phase,outcome,coalesce(error_code,'NULL') from operation_services where stack_name='$stack' order by service_name" || \
		! printf '%s\n' $'seed\trunning\tsuccess\tNULL' \
			$'web\trunning\tsuccess\tNULL' \
			| cmp -s - "$G2_RAW/children.after" || \
		! g2_capture_engine_sessions "$G2_RAW/engine.after" || \
		! cmp -s "$sessions" "$G2_RAW/engine.after" || \
		! cmp -s "$G2_RAW/starts.before" "$starts"; then
		g2_fail_case g2.fault.before-commit "cold recovery duplicated an effect or did not terminalize only the ambiguous parent"
		return 1
	fi
	sleep 1
	if ! g2_capture_engine_sessions "$G2_RAW/engine.stable" || \
		! cmp -s "$sessions" "$G2_RAW/engine.stable" || \
		! cmp -s "$G2_RAW/starts.before" "$starts" || \
		! g2_capture_runtime_evidence before-commit-recovered || \
		! g2_stop_daemon TERM || \
		! g2_exact_kill_sessions "$cleanup_sessions" before-commit-cleanup; then
		g2_fail_case g2.fault.before-commit "ambiguous sessions changed or exact reverse-DAG cleanup failed"
		return 1
	fi
	printf '%s\t%s\t%s\t%s\n' before-commit \
		"both services running; parent uncommitted" \
		"one failed/unknown parent; successful child journals retained; two unknown/installed services and unchanged sessions" \
		"two exact recorded sessions in reverse DAG" >>"$G2_MATRIX_FILE"
	device_result g2.fault.before-commit PASS 0 \
		"parent-only cold recovery terminalized one uncommitted operation, created no new operation/effect, and preserved both ambiguous sessions for exact cleanup" - -
	g2_dump_case
}

g2_fault_down_case() {
	local stack=g2-fault-down manifest port unused sessions cli_pid
	g2_new_case fault-down || { g2_fail_case g2.fault.during-down "cannot create case"; return 1; }
	read -r port unused < <(g2_two_ports) || return 1
	manifest=$G2_PROJECT/stack.yaml
	g2_write_stack_manifest "$stack" "$G2_ARCHIVE_V1" "$port" "down-$G2_RUN_ID" "$manifest" || return 1
	g2_allow_before during_down || return 1
	if ! g2_start_daemon fault || ! g2_cli up up "$manifest"; then
		g2_fail_case g2.fault.during-down "stack did not reach running before down fault"
		return 1
	fi
	sessions=$G2_RAW/sessions.before
	g2_db_rows "$sessions" \
		"select session_id from services where stack_name='$stack' order by case name when 'web' then 0 else 1 end" || return 1
	g2_capture_engine_sessions "$G2_RAW/engine.before" || return 1
	if ! g2_cli_background interrupted-down down "$stack"; then
		g2_fail_case g2.fault.during-down "fault request client could not be qualified"
		return 1
	fi
	cli_pid=$G2_LAST_CLI_PID
	if ! g2_wait_file "$G2_FAULT/during_down.reached" || ! g2_stop_daemon KILL; then
		g2_fail_case g2.fault.during-down "during-down checkpoint or daemon kill failed"
		return 1
	fi
	if ! g2_wait_interrupted_cli "$cli_pid" interrupted-down; then
		g2_fail_case g2.fault.during-down "fault request client did not terminate with the expected failure"
		return 1
	fi
	if ! g2_start_daemon normal || ! g2_cli recovered status "$stack" || \
		! jq -e '.desired_state == "stopped" and .observed_state == "unknown" and
			any(.services[]; .observed_state == "unknown")' "$G2_RAW/recovered.stdout" >/dev/null || \
		! g2_capture_engine_sessions "$G2_RAW/engine.after" || \
		! sort -n "$sessions" | cmp -s - "$G2_RAW/engine.after"; then
		g2_fail_case g2.fault.during-down "cold recovery did not preserve both pre-effect sessions"
		return 1
	fi
	if ! g2_stop_daemon TERM || ! g2_exact_kill_sessions "$sessions" down-cleanup; then
		g2_fail_case g2.fault.during-down "exact reverse-order cleanup failed"
		return 1
	fi
	printf '%s\t%s\t%s\t%s\n' during-down "stop intent durable; kill not invoked" \
		"unknown; two unchanged sessions" "two exact recorded sessions" >>"$G2_MATRIX_FILE"
	device_result g2.fault.during-down PASS 0 \
		"cold recovery did not infer stop or retry it; both exact sessions were left for explicit cleanup" - -
	g2_dump_case
}

g2_fault_backoff_case() {
	local stack=g2-fault-backoff manifest iteration running=0 starts failures
	g2_new_case fault-backoff || { g2_fail_case g2.fault.during-backoff "cannot create case"; return 1; }
	manifest=$G2_PROJECT/stack.yaml
	starts=$G2_PROJECT/events-$stack/start.tsv
	failures=$G2_PROJECT/events-$stack/failure.tsv
	g2_write_restart_manifest "$stack" recover-once on-failure "$manifest" || return 1
	g2_allow_before during_backoff || return 1
	if ! g2_start_daemon fault || ! g2_cli up up "$manifest"; then
		g2_fail_case g2.fault.during-backoff "recover-once service did not initially qualify"
		return 1
	fi
	if ! g2_wait_file "$G2_FAULT/during_backoff.reached"; then
		g2_fail_case g2.fault.during-backoff "during-backoff checkpoint was not reached"
		return 1
	fi
	if ! g2_db_rows "$G2_RAW/backoff.before" \
		"select observed_state,effect_phase,restart_attempts,(restart_window_started_at is not null),(next_restart_at is not null),coalesce(session_id,'NULL'),coalesce(child_pid,'NULL'),coalesce(child_starttime,'NULL'),coalesce(boot_id,'NULL') from services where stack_name='$stack' and name='app'" || \
		! printf '%s\n' \
			$'restarting\tbackoff\t1\t1\t1\tNULL\tNULL\tNULL\tNULL' \
			| cmp -s - "$G2_RAW/backoff.before" || \
		[[ ! -f $failures ]] || [[ $(wc -l <"$failures") -ne 1 ]] || \
		! g2_capture_engine_sessions "$G2_RAW/engine.backoff" || \
		[[ -s $G2_RAW/engine.backoff ]] || ! g2_stop_daemon KILL; then
		g2_fail_case g2.fault.during-backoff "durable no-child backoff was not reached"
		return 1
	fi
	if ! g2_start_daemon normal; then g2_fail_case g2.fault.during-backoff "cold daemon did not start"; return 1; fi
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		if g2_cli recovered status "$stack" && jq -e '
			.services[0].observed_state == "running" and .services[0].restart_attempts == 1
		' "$G2_RAW/recovered.stdout" >/dev/null; then running=1; break; fi
		sleep 0.1
	done
	if ((running == 0)) || [[ ! -f $starts || $(wc -l <"$starts") -ne 2 ]] || \
		! g2_capture_engine_sessions "$G2_RAW/engine.recovered" || \
		[[ $(wc -l <"$G2_RAW/engine.recovered") -ne 1 ]] || \
		! g2_cli down down "$stack" || ! g2_stop_daemon TERM; then
		g2_fail_case g2.fault.during-backoff "cold recovery did not resume exactly one proven-safe retry"
		return 1
	fi
	printf '%s\t%s\t%s\t%s\n' during-backoff "previous process absent; retry durably scheduled" \
		"one recovered session; two total starts" "ordinary down" >>"$G2_MATRIX_FILE"
	device_result g2.fault.during-backoff PASS 0 \
		"cold recovery resumed one durable retry only after prior absence was proven" - -
	g2_dump_case
}

g2_cleanup() {
	local cleanup_rc=0 index output daemon_rc cli_rc cli_state
	if [[ $G2_CLEANUP_STATE == done ]]; then return 0; fi
	G2_CLEANUP_STATE=running
	g2_install_deferred_signal_handlers
	if [[ -n $G2_ACTIVE_DAEMON_PID ]]; then
		if g2_daemon_identity_matches; then
			if kill -TERM "$G2_ACTIVE_DAEMON_PID" 2>/dev/null; then
				g2_wait_pid "$G2_ACTIVE_DAEMON_PID" 240 >/dev/null 2>&1
				daemon_rc=$?
				if [[ $daemon_rc -eq 124 ]]; then
					if g2_daemon_identity_matches; then
						kill -KILL "$G2_ACTIVE_DAEMON_PID" 2>/dev/null || true
						g2_wait_pid "$G2_ACTIVE_DAEMON_PID" 200 >/dev/null 2>&1 || true
					fi
					cleanup_rc=1
				elif [[ $daemon_rc -ne 0 ]]; then
					cleanup_rc=1
				fi
			else
				cleanup_rc=1
			fi
		else
			cleanup_rc=1
		fi
		G2_ACTIVE_DAEMON_PID=
	fi
	if [[ -n $G2_ACTIVE_CLI_PID ]]; then
		cli_rc=124
		cli_state=$(g2_proc_state "$G2_ACTIVE_CLI_PID" 2>/dev/null || true)
		if [[ $cli_state == Z || $cli_state == X || $cli_state == x ]] || \
			! kill -0 "$G2_ACTIVE_CLI_PID" 2>/dev/null; then
			wait "$G2_ACTIVE_CLI_PID" 2>/dev/null
			cli_rc=$?
		elif g2_cli_identity_matches; then
			g2_intent signal-cli-TERM "$G2_ACTIVE_CLI_PID" || cleanup_rc=1
			kill -TERM "$G2_ACTIVE_CLI_PID" 2>/dev/null || cleanup_rc=1
			g2_wait_pid "$G2_ACTIVE_CLI_PID" 100 >/dev/null 2>&1
			cli_rc=$?
			if [[ $cli_rc -eq 124 ]] && g2_cli_identity_matches; then
				g2_intent signal-cli-KILL "$G2_ACTIVE_CLI_PID" || cleanup_rc=1
				kill -KILL "$G2_ACTIVE_CLI_PID" 2>/dev/null || cleanup_rc=1
				g2_wait_pid "$G2_ACTIVE_CLI_PID" 100 >/dev/null 2>&1
				cli_rc=$?
			fi
		else
			cleanup_rc=1
		fi
		if [[ $cli_rc -eq 124 ]]; then
			cleanup_rc=1
		else
			G2_ACTIVE_CLI_PID=
			G2_ACTIVE_CLI_STARTTIME=
			G2_ACTIVE_CLI_BOOT_ID=
			G2_LAST_CLI_PID=
		fi
	fi
	for ((index = 0; index < ${#G2_PREFIXES[@]}; index += 1)); do
		output=$DEVICE_EVIDENCE_DIR/cleanup-sessions-$((index + 1)).txt
		if ! g2_pd_for "${G2_PREFIXES[$index]}" "${G2_HOMES[$index]}" ps --quiet \
			>"$output" 2>"$output.stderr" || [[ -s $output ]]; then
			cleanup_rc=1
		fi
	done
	if ((G2_PREFLIGHT_DONE)); then
		env TERMUX__PREFIX="$G2_REAL_PREFIX" TERMUX__HOME="$G2_REAL_HOME" \
			PD_PROOT_BIN="$G2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
			COLUMNS=240 PATH="$G2_REAL_PREFIX/bin:$G2_REAL_PATH" \
			proot-distro list --quiet >"$G2_REAL_AFTER" \
			2>"$DEVICE_STDIO_DIR/postflight.real-list.stderr" || cleanup_rc=1
		if cmp -s "$G2_REAL_BEFORE" "$G2_REAL_AFTER"; then
			device_result postflight.real-runtime PASS 0 "real container inventory is unchanged" - -
		else
			device_result postflight.real-runtime FAIL 1 \
				"real container inventory changed or could not be observed" - -
			cleanup_rc=1
		fi
	fi
	if ((cleanup_rc != 0)); then
		G2_PRESERVE_RUNTIME=1
		G2_CLEANUP_FAILURES=$((G2_CLEANUP_FAILURES + 1))
	fi
	G2_CLEANUP_STATE=done
	return "$cleanup_rc"
}

g2_on_exit() {
	local original_rc=$? finish_rc=0 final_rc cleanup_rc=0
	g2_install_deferred_signal_handlers
	trap - EXIT
	if ((original_rc != 0 || G2_PRESERVE_RUNTIME || G2_CLEANUP_FAILURES > 0)) && \
		[[ -n $G2_RAW && -d $G2_RAW ]]; then
		g2_capture_runtime_evidence pre-cleanup || true
		g2_dump_case || true
	fi
	if ! g2_cleanup; then
		cleanup_rc=1
		original_rc=1
	fi
	if ((cleanup_rc != 0)) && [[ -n $G2_RAW && -d $G2_RAW ]]; then
		g2_capture_runtime_evidence post-cleanup-ambiguity || true
		g2_dump_case || true
	fi
	if ((G2_CLEANUP_FAILURES > 0)); then
		device_result cleanup.objects FAIL 1 "cleanup was ambiguous; private runtime preserved" - -
	else
		device_result cleanup.objects PASS 0 "all exact test sessions drained" - -
	fi
	if ((G2_PRESERVE_RUNTIME)); then
		device_metadata preserved_runtime "$DEVICE_RUNTIME_DIR"
		DEVICE_RUNTIME_DIR=
	fi
	device_finish || finish_rc=1
	device_cleanup
	if ((G2_DEFERRED_SIGNAL != 0)); then final_rc=$G2_DEFERRED_SIGNAL
	elif ((original_rc != 0 || finish_rc != 0 || DEVICE_FAILURE_COUNT > 0)); then final_rc=1
	else final_rc=0
	fi
	exit "$final_rc"
}

trap g2_on_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

preflight_ok=1
if [[ -z $G2_REAL_PREFIX || $G2_REAL_PREFIX != /* || -z $G2_REAL_HOME ]]; then preflight_ok=0; fi
if ! g2_is_within_app_files "$DEVICE_RUN_DIR" || \
	! g2_is_within_app_files "$DEVICE_RUNTIME_DIR"; then preflight_ok=0; fi
if [[ $binary != /* || ! -x $binary || -L $binary ]]; then preflight_ok=0; fi
if [[ $archive_v1 != /* || ! -f $archive_v1 || -L $archive_v1 ]]; then preflight_ok=0; fi
if [[ $archive_v2 != /* || ! -f $archive_v2 || -L $archive_v2 ]]; then preflight_ok=0; fi
for provenance_file in "$G2_REPO_ROOT/Cargo.lock" "$G2_REPO_ROOT/Cargo.toml" \
	"$SCRIPT_DIR/g2.sh" "$SCRIPT_DIR/lib.sh" "$SCRIPT_DIR/README.md" \
	"$FIXTURE_DIR/verify-oci.sh" "$FIXTURE_DIR/Containerfile" "$FIXTURE_DIR/worker"; do
	[[ -f $provenance_file && ! -L $provenance_file ]] || preflight_ok=0
done
for hash in "$archive_v1_sha" "$archive_v2_sha"; do
	[[ $hash =~ ^[0-9a-f]{64}$ ]] || preflight_ok=0
done
if [[ $archive_v1_sha == "$archive_v2_sha" ]]; then preflight_ok=0; fi
for command_name in proot-distro proot python3 jq sha256sum sync awk sort cmp find git stat getprop; do
	command -v "$command_name" >/dev/null 2>&1 || preflight_ok=0
done
if [[ $(uname -m) != aarch64 ]]; then preflight_ok=0; fi
if [[ $(stat -c '%a' "$DEVICE_RUN_DIR" 2>/dev/null) != 700 || \
	$(stat -c '%a' "$DEVICE_RUNTIME_DIR" 2>/dev/null) != 700 ]]; then preflight_ok=0; fi
if ((preflight_ok)); then G2_BOOT_ID=$(< /proc/sys/kernel/random/boot_id) || preflight_ok=0; fi
if ((preflight_ok)); then
	git -C "$G2_REPO_ROOT" rev-parse --show-toplevel \
		>"$DEVICE_STDIO_DIR/preflight.git-root.stdout" \
		2>"$DEVICE_STDIO_DIR/preflight.git-root.stderr" || preflight_ok=0
fi
if ((preflight_ok)); then
	git_root=$(cd -- "$(<"$DEVICE_STDIO_DIR/preflight.git-root.stdout")" 2>/dev/null && pwd -P) || preflight_ok=0
	[[ ${git_root:-} == "$G2_REPO_ROOT" ]] || preflight_ok=0
fi
if ((preflight_ok)); then
	G2_SOURCE_COMMIT=$(git -C "$G2_REPO_ROOT" rev-parse --verify HEAD \
		2>"$DEVICE_STDIO_DIR/preflight.git-commit.stderr") || preflight_ok=0
	[[ $G2_SOURCE_COMMIT =~ ^[0-9a-f]{40,64}$ ]] || preflight_ok=0
fi
if ((preflight_ok)); then
	git -C "$G2_REPO_ROOT" status --porcelain=v1 --untracked-files=all \
		>"$DEVICE_STDIO_DIR/preflight.git-status.stdout" \
		2>"$DEVICE_STDIO_DIR/preflight.git-status.stderr" || preflight_ok=0
	if [[ -s $DEVICE_STDIO_DIR/preflight.git-status.stdout ]]; then
		G2_SOURCE_STATUS=dirty
		preflight_ok=0
	else
		G2_SOURCE_STATUS=clean
	fi
fi
if ((preflight_ok)); then
	device_metadata source_commit "$G2_SOURCE_COMMIT"
	device_metadata source_status "$G2_SOURCE_STATUS"
	device_metadata repository_root "$G2_REPO_ROOT"
	device_metadata app_files_root "$G2_APP_FILES_ROOT"
	device_metadata output_root "$g2_canonical_output_root"
	device_metadata runtime_root "$g2_canonical_runtime_root"
	device_metadata binary "$binary"
	device_metadata binary_sha256 "$(sha256sum "$binary" | awk '{print $1}')"
	device_metadata cargo_lock_sha256 "$(sha256sum "$G2_REPO_ROOT/Cargo.lock" | awk '{print $1}')"
	device_metadata cargo_manifest_sha256 "$(sha256sum "$G2_REPO_ROOT/Cargo.toml" | awk '{print $1}')"
	device_metadata harness_sha256 "$(sha256sum "$SCRIPT_DIR/g2.sh" | awk '{print $1}')"
	device_metadata harness_lib_sha256 "$(sha256sum "$SCRIPT_DIR/lib.sh" | awk '{print $1}')"
	device_metadata device_readme_sha256 "$(sha256sum "$SCRIPT_DIR/README.md" | awk '{print $1}')"
	device_metadata fixture_verifier_sha256 "$(sha256sum "$FIXTURE_DIR/verify-oci.sh" | awk '{print $1}')"
	device_metadata fixture_containerfile_sha256 "$(sha256sum "$FIXTURE_DIR/Containerfile" | awk '{print $1}')"
	device_metadata fixture_worker_sha256 "$(sha256sum "$FIXTURE_DIR/worker" | awk '{print $1}')"
	device_metadata fixture_revision 3
	device_metadata archive_v1_sha256 "$archive_v1_sha"
	device_metadata archive_v2_sha256 "$archive_v2_sha"
	device_metadata architecture "$(uname -m)"
	device_metadata kernel "$(uname -srvmo)"
	device_metadata android_api "$(getprop ro.build.version.sdk 2>/dev/null || printf unknown)"
	device_metadata android_fingerprint "$(getprop ro.build.fingerprint 2>/dev/null || printf unknown)"
	device_metadata git "$(git --version 2>/dev/null || printf unknown)"
	device_metadata proot_distro "$(dpkg-query -W -f='${Version}' proot-distro 2>/dev/null || printf unknown)"
	device_metadata proot "$(dpkg-query -W -f='${Version}' proot 2>/dev/null || printf unknown)"
fi
if ((preflight_ok)); then
	cp -- "$archive_v1" "$G2_ARCHIVE_V1" && cp -- "$archive_v2" "$G2_ARCHIVE_V2" || preflight_ok=0
	chmod 0600 "$G2_ARCHIVE_V1" "$G2_ARCHIVE_V2" || preflight_ok=0
fi
if ((preflight_ok)); then
	bash "$FIXTURE_DIR/verify-oci.sh" "$G2_ARCHIVE_V1" "$archive_v1_sha" v1 \
		>"$G2_ARCHIVE_V1_REPORT" \
		2>"$DEVICE_STDIO_DIR/preflight.archive-v1.stderr" || preflight_ok=0
fi
if ((preflight_ok)); then
	bash "$FIXTURE_DIR/verify-oci.sh" "$G2_ARCHIVE_V2" "$archive_v2_sha" v2 \
		>"$G2_ARCHIVE_V2_REPORT" \
		2>"$DEVICE_STDIO_DIR/preflight.archive-v2.stderr" || preflight_ok=0
fi
if ((preflight_ok)); then
	G2_MANIFEST_V1_DIGEST=$(awk -F '\t' '$1 == "manifest_digest" { print $2 }' \
		"$G2_ARCHIVE_V1_REPORT") || preflight_ok=0
	G2_MANIFEST_V2_DIGEST=$(awk -F '\t' '$1 == "manifest_digest" { print $2 }' \
		"$G2_ARCHIVE_V2_REPORT") || preflight_ok=0
	[[ $G2_MANIFEST_V1_DIGEST =~ ^sha256:[0-9a-f]{64}$ && \
		$G2_MANIFEST_V2_DIGEST =~ ^sha256:[0-9a-f]{64}$ && \
		$G2_MANIFEST_V1_DIGEST != "$G2_MANIFEST_V2_DIGEST" ]] || preflight_ok=0
fi
if ((preflight_ok)); then
	env TERMUX__PREFIX="$G2_REAL_PREFIX" TERMUX__HOME="$G2_REAL_HOME" \
		PD_PROOT_BIN="$G2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		COLUMNS=240 PATH="$G2_REAL_PREFIX/bin:$G2_REAL_PATH" \
		proot-distro list --quiet >"$G2_REAL_BEFORE" \
		2>"$DEVICE_STDIO_DIR/preflight.real-list.stderr" || preflight_ok=0
fi
if ((preflight_ok)); then
	G2_PREFLIGHT_DONE=1
	device_metadata manifest_v1_digest "$G2_MANIFEST_V1_DIGEST"
	device_metadata manifest_v2_digest "$G2_MANIFEST_V2_DIGEST"
	device_result preflight PASS 0 \
		"aarch64, debug binary, tools, and two independently pinned OCI fixtures qualified" - -
else
	device_result preflight FAIL 1 "preflight or OCI qualification failed" - \
		"stdout-stderr/preflight.archive-v1.stderr"
	exit 1
fi

g2_require_case normal g2_normal_case || exit 1
g2_require_case restart-cap g2_restart_cap_case || exit 1
g2_require_case fault-between g2_fault_between_case || exit 1
g2_require_case fault-before-commit g2_fault_before_commit_case || exit 1
g2_require_case fault-down g2_fault_down_case || exit 1
g2_require_case fault-backoff g2_fault_backoff_case || exit 1
