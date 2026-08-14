#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
FIXTURE_DIR=$SCRIPT_DIR/fixtures/s5

DEVICE_PHASE=S5
DEVICE_RUN_LABEL=termux-stacks-s5
DEVICE_RUNTIME_LABEL=txs-s5
DEVICE_HARNESS_VERSION=1
DEVICE_AUTOMATIC_SCOPE=$'The harness exercised the debug-only S5 recovery checkpoints in fresh synthetic\nTermux Stacks and proot-distro prefixes. It never targeted a real container,\nused only exact persisted session IDs for manual recovery, and preserved every\ncase whose process, engine, database, or containment state was ambiguous.'

# shellcheck source=tests/device/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
	cat <<'EOF'
Usage:
  bash tests/device/s5.sh --binary ABSOLUTE_DEBUG_BINARY \
    --archive ABSOLUTE_OCI_ARCHIVE --archive-sha256 LOWERCASE_SHA256 \
    [--output-root ABSOLUTE_DIR] [--unknown-cycles 1..20]

The supplied binary must be a debug build containing the controlled S5 fault
checkpoints. The default runs 20 after-start kill/restart cycles, in addition
to the normal lifecycle and every other required fault point. Release builds
intentionally ignore TERMUX_STACKS_FAULT_DIR and cannot run this harness.
EOF
}

binary=
archive=
archive_sha256=
output_root=
unknown_cycles=20
while (($# > 0)); do
	case $1 in
		--binary)
			[[ $# -ge 2 ]] || { device_error "--binary requires a value"; exit 2; }
			binary=$2
			shift 2
			;;
		--archive)
			[[ $# -ge 2 ]] || { device_error "--archive requires a value"; exit 2; }
			archive=$2
			shift 2
			;;
		--archive-sha256)
			[[ $# -ge 2 ]] || { device_error "--archive-sha256 requires a value"; exit 2; }
			archive_sha256=$2
			shift 2
			;;
		--output-root)
			[[ $# -ge 2 ]] || { device_error "--output-root requires a value"; exit 2; }
			output_root=$2
			shift 2
			;;
		--unknown-cycles)
			[[ $# -ge 2 ]] || { device_error "--unknown-cycles requires a value"; exit 2; }
			unknown_cycles=$2
			shift 2
			;;
		-h | --help)
			usage
			exit 0
			;;
		*)
			device_error "unknown argument: $1"
			usage >&2
			exit 2
			;;
	esac
done

if [[ -z $binary || -z $archive || -z $archive_sha256 ]]; then
	usage >&2
	exit 2
fi
if [[ ! $unknown_cycles =~ ^([1-9]|1[0-9]|20)$ ]]; then
	device_error "--unknown-cycles must be an integer from 1 through 20"
	exit 2
fi
unknown_cycles=$((10#$unknown_cycles))

device_init "$output_root" || exit $?

S5_CASES_DIR=$DEVICE_EVIDENCE_DIR/cases
S5_FAULTS_FILE=$DEVICE_EVIDENCE_DIR/faults.tsv
S5_INTENT_FILE=$DEVICE_EVIDENCE_DIR/intent.tsv
S5_REAL_BEFORE=$DEVICE_EVIDENCE_DIR/real-containers.before
S5_REAL_AFTER=$DEVICE_EVIDENCE_DIR/real-containers.after
S5_ARCHIVE_REPORT=$DEVICE_EVIDENCE_DIR/archive.tsv
mkdir -m 0700 -- "$S5_CASES_DIR"
printf 'case\tcheckpoint\tcycle\texpected\tobserved\talias_before\talias_after\tcleanup\n' >"$S5_FAULTS_FILE"
printf 'time_utc\taction\ttarget\n' >"$S5_INTENT_FILE"

S5_REAL_PREFIX=${PREFIX:-}
S5_REAL_HOME=${HOME:-}
S5_REAL_PATH=${PATH:-}
S5_ARCHIVE_SNAPSHOT=$DEVICE_RUNTIME_DIR/input.oci.tar
S5_BOOT_ID=
S5_CLEANUP_STATE=pending
S5_CLEANUP_FAILURES=0
S5_DEFERRED_SIGNAL=0
S5_PRESERVE_RUNTIME=0
S5_ACTIVE_DAEMON_PID=
S5_ACTIVE_DAEMON_STARTTIME=
S5_ACTIVE_DAEMON_BOOT_ID=
S5_CASE=
S5_CASE_ROOT=
S5_CASE_PREFIX=
S5_CASE_HOME=
S5_CASE_FAULT=
S5_CASE_MANIFEST=
S5_CASE_RAW=
S5_STACK=
S5_CASE_ALIAS=
S5_CASE_CHILD_PID=
S5_LAST_CLI_PID=
S5_DAEMON_SEQUENCE=0
S5_CASE_SEQUENCE=0
S5_STORAGE_PAGE_LIMIT=

s5_defer_signal() {
	local code=$1
	if ((S5_DEFERRED_SIGNAL == 0)); then S5_DEFERRED_SIGNAL=$code; fi
}

s5_install_deferred_signal_handlers() {
	trap 's5_defer_signal 129' HUP
	trap 's5_defer_signal 130' INT
	trap 's5_defer_signal 143' TERM
}

s5_intent() {
	local action=$1 target=$2
	printf '%s\t%s\t%s\n' \
		"$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
		"$(device_sanitize_tsv "$action")" \
		"$(device_sanitize_tsv "$target")" >>"$S5_INTENT_FILE" || return 1
	sync -f "$S5_INTENT_FILE"
}

s5_proc_starttime() {
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

s5_daemon_identity_matches() {
	local current_starttime current_boot
	[[ $S5_ACTIVE_DAEMON_PID =~ ^[1-9][0-9]*$ ]] || return 1
	current_boot=$(< /proc/sys/kernel/random/boot_id) || return 1
	[[ $current_boot == "$S5_ACTIVE_DAEMON_BOOT_ID" ]] || return 1
	current_starttime=$(s5_proc_starttime "$S5_ACTIVE_DAEMON_PID") || return 1
	[[ $current_starttime == "$S5_ACTIVE_DAEMON_STARTTIME" ]]
}

s5_env() {
	env \
		PREFIX="$S5_CASE_PREFIX" \
		TERMUX__PREFIX="$S5_CASE_PREFIX" \
		TERMUX__HOME="$S5_CASE_HOME" \
		PD_PROOT_BIN="$S5_REAL_PREFIX/bin/proot" \
		PD_FORCE_NO_COLORS=true \
		COLUMNS=240 \
		PATH="$S5_REAL_PREFIX/bin:$S5_REAL_PATH" \
		"$@"
}

s5_pd() {
	s5_env proot-distro "$@"
}

s5_wait_pid() {
	local pid=$1 limit=${2:-100} iteration
	for ((iteration = 0; iteration < limit; iteration += 1)); do
		kill -0 "$pid" 2>/dev/null || {
			wait "$pid" 2>/dev/null
			return $?
		}
		sleep 0.05
	done
	return 124
}

s5_wait_file() {
	local path=$1 iteration
	for ((iteration = 0; iteration < 600; iteration += 1)); do
		[[ -f $path && ! -L $path ]] && return 0
		if [[ -n $S5_ACTIVE_DAEMON_PID ]] && ! kill -0 "$S5_ACTIVE_DAEMON_PID" 2>/dev/null; then
			return 1
		fi
		sleep 0.05
	done
	return 1
}

s5_new_case() {
	local label=$1 stack=$2
	S5_CASE_SEQUENCE=$((S5_CASE_SEQUENCE + 1))
	S5_CASE=$label
	S5_CASE_ROOT=$DEVICE_RUNTIME_DIR/c$S5_CASE_SEQUENCE
	S5_CASE_PREFIX=$S5_CASE_ROOT/p
	S5_CASE_HOME=$S5_CASE_ROOT/h
	S5_CASE_FAULT=$S5_CASE_ROOT/f
	S5_CASE_MANIFEST=$S5_CASE_ROOT/stack.yaml
	S5_CASE_RAW=$S5_CASES_DIR/$label
	S5_STACK=$stack
	S5_CASE_ALIAS=
	S5_CASE_CHILD_PID=
	S5_DAEMON_SEQUENCE=0
	mkdir -m 0700 -- \
		"$S5_CASE_ROOT" "$S5_CASE_PREFIX" "$S5_CASE_HOME" "$S5_CASE_FAULT" "$S5_CASE_RAW" || \
		return 1
	printf '%s\n' \
		'apiVersion: termux-stacks/v1alpha1' \
		'kind: Stack' \
		'metadata:' \
		"  name: $stack" \
		'services:' \
		'  app:' \
		"    image: $S5_ARCHIVE_SNAPSHOT" \
		'    command:' \
		'      - /bin/sh' \
		'      - -c' \
		'      - "i=0; while [ $i -lt 180 ]; do echo tick; i=$((i + 1)); sleep 1; done"' >"$S5_CASE_MANIFEST" || return 1
	chmod 0600 "$S5_CASE_MANIFEST" || return 1
	cp -- "$S5_CASE_MANIFEST" "$S5_CASE_RAW/manifest.yaml" || return 1
}

s5_continue_before() {
	local target=$1 checkpoint
	for checkpoint in before_intent after_intent after_install after_start before_commit; do
		[[ $checkpoint == "$target" ]] && return 0
		: >"$S5_CASE_FAULT/$checkpoint.continue" || return 1
	done
	[[ $target == during_down ]]
}

s5_start_daemon() {
	local fault=${1:-none} stdout stderr pid iteration starttime=
	S5_DAEMON_SEQUENCE=$((S5_DAEMON_SEQUENCE + 1))
	stdout=$S5_CASE_RAW/daemon-$S5_DAEMON_SEQUENCE.stdout
	stderr=$S5_CASE_RAW/daemon-$S5_DAEMON_SEQUENCE.stderr
	if [[ $fault == fault ]]; then
		env -u TERMUX_STACKS_SQLITE_MAX_PAGES \
			PREFIX="$S5_CASE_PREFIX" \
			TERMUX__PREFIX="$S5_CASE_PREFIX" \
			TERMUX__HOME="$S5_CASE_HOME" \
			PD_PROOT_BIN="$S5_REAL_PREFIX/bin/proot" \
			PD_FORCE_NO_COLORS=true COLUMNS=240 \
			PATH="$S5_REAL_PREFIX/bin:$S5_REAL_PATH" \
			TERMUX_STACKS_FAULT_DIR="$S5_CASE_FAULT" \
			"$binary" daemon >"$stdout" 2>"$stderr" &
	elif [[ $fault == sqlite_full ]]; then
		[[ $S5_STORAGE_PAGE_LIMIT =~ ^[1-9][0-9]*$ ]] || return 1
		env -u TERMUX_STACKS_FAULT_DIR \
			PREFIX="$S5_CASE_PREFIX" \
			TERMUX__PREFIX="$S5_CASE_PREFIX" \
			TERMUX__HOME="$S5_CASE_HOME" \
			PD_PROOT_BIN="$S5_REAL_PREFIX/bin/proot" \
			PD_FORCE_NO_COLORS=true COLUMNS=240 \
			PATH="$S5_REAL_PREFIX/bin:$S5_REAL_PATH" \
			TERMUX_STACKS_SQLITE_MAX_PAGES="$S5_STORAGE_PAGE_LIMIT" \
			"$binary" daemon >"$stdout" 2>"$stderr" &
	else
		env -u TERMUX_STACKS_FAULT_DIR -u TERMUX_STACKS_SQLITE_MAX_PAGES \
			PREFIX="$S5_CASE_PREFIX" \
			TERMUX__PREFIX="$S5_CASE_PREFIX" \
			TERMUX__HOME="$S5_CASE_HOME" \
			PD_PROOT_BIN="$S5_REAL_PREFIX/bin/proot" \
			PD_FORCE_NO_COLORS=true COLUMNS=240 \
			PATH="$S5_REAL_PREFIX/bin:$S5_REAL_PATH" \
			"$binary" daemon >"$stdout" 2>"$stderr" &
	fi
	pid=$!
	S5_ACTIVE_DAEMON_PID=$pid
	S5_ACTIVE_DAEMON_BOOT_ID=$S5_BOOT_ID
	for ((iteration = 0; iteration < 50; iteration += 1)); do
		starttime=$(s5_proc_starttime "$pid") && break
		kill -0 "$pid" 2>/dev/null || return 1
		sleep 0.02
	done
	[[ -n $starttime ]] || return 1
	S5_ACTIVE_DAEMON_STARTTIME=$starttime
	device_wait_for_socket "$pid" "$S5_CASE_PREFIX/var/run/termux-stacks/daemon.sock" || return 1
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		grep -F ' daemon ready ' "$stdout" >/dev/null 2>&1 && break
		kill -0 "$pid" 2>/dev/null || return 1
		sleep 0.02
	done
	grep -F ' daemon ready ' "$stdout" >/dev/null 2>&1 || return 1
	s5_daemon_identity_matches
}

s5_stop_daemon() {
	local signal=${1:-TERM} pid=$S5_ACTIVE_DAEMON_PID rc
	[[ -n $pid ]] || return 0
	s5_daemon_identity_matches || return 1
	s5_intent "signal-daemon-$signal" "$pid" || return 1
	kill -"$signal" "$pid" || return 1
	s5_wait_pid "$pid" 200
	rc=$?
	if [[ $signal == KILL ]]; then
		[[ $rc -eq 137 || $rc -eq 0 ]] || return 1
	else
		[[ $rc -eq 0 ]] || return 1
	fi
	S5_ACTIVE_DAEMON_PID=
	S5_ACTIVE_DAEMON_STARTTIME=
	S5_ACTIVE_DAEMON_BOOT_ID=
}

s5_cli() {
	local label=$1
	shift
	s5_env "$binary" "$@" >"$S5_CASE_RAW/$label.stdout" 2>"$S5_CASE_RAW/$label.stderr"
}

s5_cli_background() {
	local label=$1
	shift
	env \
		PREFIX="$S5_CASE_PREFIX" \
		TERMUX__PREFIX="$S5_CASE_PREFIX" \
		TERMUX__HOME="$S5_CASE_HOME" \
		PD_PROOT_BIN="$S5_REAL_PREFIX/bin/proot" \
		PD_FORCE_NO_COLORS=true COLUMNS=240 \
		PATH="$S5_REAL_PREFIX/bin:$S5_REAL_PATH" \
		"$binary" "$@" >"$S5_CASE_RAW/$label.stdout" 2>"$S5_CASE_RAW/$label.stderr" &
	S5_LAST_CLI_PID=$!
}

s5_db_value() {
	local sql=$1
	python3 - "$S5_CASE_PREFIX/var/lib/termux-stacks/state.db" "$sql" <<'PY'
import sqlite3
import sys

row = sqlite3.connect(sys.argv[1]).execute(sys.argv[2]).fetchone()
if row is None or row[0] is None:
    raise SystemExit(1)
print(row[0])
PY
}

s5_db_count() {
	local sql=$1
	s5_db_value "$sql"
}

s5_assert_status() {
	local file=$1 service_state=$2 rootfs_state=$3
	jq -e --arg service "$service_state" --arg rootfs "$rootfs_state" \
		'.service_state == $service and .rootfs_state == $rootfs' "$file" >/dev/null
}

s5_wait_log_contains() {
	local path=$1 pattern=$2 iteration
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		grep -F "$pattern" "$path" >/dev/null 2>&1 && return 0
		sleep 0.05
	done
	return 1
}

s5_dump_case() {
	local database=$S5_CASE_PREFIX/var/lib/termux-stacks/state.db
	[[ -f $database && ! -L $database ]] || return 0
	python3 - "$database" >"$S5_CASE_RAW/database.txt" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
print("integrity_check\t" + connection.execute("pragma integrity_check").fetchone()[0])
for table in ("meta", "stacks", "services", "operations"):
    print(f"[{table}]")
    for row in connection.execute(f"select * from {table} order by rowid"):
        print("\t".join("NULL" if value is None else str(value) for value in row))
PY
	local log
	for log in \
		"$S5_CASE_PREFIX/var/lib/termux-stacks/logs"/*/*.stdout.log \
		"$S5_CASE_PREFIX/var/lib/termux-stacks/logs"/*/*.stderr.log; do
		[[ -f $log && ! -L $log ]] || continue
		cp -- "$log" "$S5_CASE_RAW/${log##*/}" || return 1
	done
}

s5_session_visible() {
	local session=$1 output=$2 iteration
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		if s5_pd ps --quiet >"$output" 2>"$output.stderr" && grep -Fx "$session" "$output" >/dev/null; then
			return 0
		fi
		sleep 0.05
	done
	return 1
}

s5_manual_unknown_cleanup() {
	local label=$1 session before after
	S5_CASE_ALIAS=$(s5_db_value "select alias from services limit 1") || return 1
	S5_CASE_CHILD_PID=$(s5_db_value "select child_pid from services limit 1") || return 1
	session=$S5_CASE_CHILD_PID
	s5_session_visible "$session" "$S5_CASE_RAW/$label.sessions-before" || return 1
	before=$(sha256sum "$S5_CASE_RAW/$label.sessions-before") || return 1
	before=${before%% *}
	if s5_cli "$label-down" down "$S5_STACK"; then
		return 1
	fi
	s5_pd ps --quiet >"$S5_CASE_RAW/$label.sessions-after-down" 2>"$S5_CASE_RAW/$label.sessions-after-down.stderr" || return 1
	after=$(sha256sum "$S5_CASE_RAW/$label.sessions-after-down") || return 1
	after=${after%% *}
	[[ $after == "$before" ]] || return 1
	s5_intent "engine-kill-exact-session" "$session" || return 1
	s5_pd kill "$session" >"$S5_CASE_RAW/$label.kill.stdout" 2>"$S5_CASE_RAW/$label.kill.stderr" || return 1
	for _ in $(seq 1 100); do
		s5_pd ps --quiet >"$S5_CASE_RAW/$label.sessions-drain" 2>"$S5_CASE_RAW/$label.sessions-drain.stderr" || return 1
		grep -Fx "$session" "$S5_CASE_RAW/$label.sessions-drain" >/dev/null || break
		sleep 0.05
	done
	grep -Fx "$session" "$S5_CASE_RAW/$label.sessions-drain" >/dev/null && return 1
	s5_intent "engine-remove-exact-alias" "$S5_CASE_ALIAS" || return 1
	s5_pd remove --quiet "$S5_CASE_ALIAS" >"$S5_CASE_RAW/$label.remove.stdout" 2>"$S5_CASE_RAW/$label.remove.stderr" || return 1
}

s5_remove_stopped_alias() {
	local alias=$1 label=$2
	[[ $alias =~ ^txs-[a-z0-9-]+$ ]] || return 1
	s5_pd ps --quiet >"$S5_CASE_RAW/$label.sessions" 2>"$S5_CASE_RAW/$label.sessions.stderr" || return 1
	[[ ! -s $S5_CASE_RAW/$label.sessions ]] || return 1
	s5_intent "engine-remove-exact-alias" "$alias" || return 1
	s5_pd remove --quiet "$alias" >"$S5_CASE_RAW/$label.remove.stdout" 2>"$S5_CASE_RAW/$label.remove.stderr" || return 1
}

s5_finish_case() {
	local outcome=$1
	s5_dump_case || outcome=FAIL
	if [[ $outcome == FAIL ]]; then
		S5_PRESERVE_RUNTIME=1
		S5_CLEANUP_FAILURES=$((S5_CLEANUP_FAILURES + 1))
	fi
}

s5_normal_case() {
	local stack=s5-normal alias alias_reused replay_count mismatch_count outcome=PASS
	s5_new_case normal "$stack" || return 1
	s5_start_daemon none || outcome=FAIL
	if [[ $outcome == PASS ]] && ! python3 "$FIXTURE_DIR/protocol_probe.py" \
		"$S5_CASE_PREFIX/var/run/termux-stacks/daemon.sock" \
		"$S5_CASE_MANIFEST" "$S5_CASE_RAW" \
		>"$S5_CASE_RAW/protocol-probe.stdout" 2>"$S5_CASE_RAW/protocol-probe.stderr"; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_cli status-after-probe status "$stack"; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_assert_status "$S5_CASE_RAW/status-after-probe.stdout" running installed; then outcome=FAIL; fi
	if [[ $outcome == PASS ]]; then
		replay_count=$(s5_db_count "select count(*) from operations where request_id = 's5-replay-1'") || outcome=FAIL
		mismatch_count=$(s5_db_count "select count(*) from operations where request_id = 's5-version-mismatch-1'") || outcome=FAIL
		[[ $replay_count == 1 && $mismatch_count == 0 ]] || outcome=FAIL
	fi
	if [[ $outcome == PASS ]]; then alias=$(s5_db_value "select alias from services limit 1") || outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_cli down-first down "$stack"; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_cli up-reuse up "$S5_CASE_MANIFEST"; then outcome=FAIL; fi
	if [[ $outcome == PASS ]]; then alias_reused=$(s5_db_value "select alias from services limit 1") || outcome=FAIL; fi
	if [[ $outcome == PASS && $alias_reused != "$alias" ]]; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_stop_daemon TERM; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_start_daemon none; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_cli status-after-restart status "$stack"; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_assert_status "$S5_CASE_RAW/status-after-restart.stdout" stopped installed; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_stop_daemon TERM; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_remove_stopped_alias "$alias" normal; then outcome=FAIL; fi
	s5_finish_case "$outcome"
	device_result lifecycle.normal "$outcome" "$([[ $outcome == PASS ]] && echo 0 || echo 1)" \
		"normal up/down, rootfs reuse, active SIGTERM shutdown, and cold restart" - -
	[[ $outcome == PASS ]]
}

s5_storage_full_case() {
	local stack=s5-storage-full alias outcome=PASS
	s5_new_case storage-full "$stack" || return 1
	cp -- "$S5_CASE_MANIFEST" "$S5_CASE_ROOT/small.yaml" || outcome=FAIL
	if [[ $outcome == PASS ]] && ! s5_start_daemon none; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_stop_daemon TERM; then outcome=FAIL; fi
	if [[ $outcome == PASS ]]; then
		python3 - "$S5_CASE_MANIFEST" "$S5_ARCHIVE_SNAPSHOT" "$stack" <<'PY' || outcome=FAIL
from pathlib import Path
import sys

manifest = "\n".join(
    [
        "apiVersion: termux-stacks/v1alpha1",
        "kind: Stack",
        "metadata:",
        f"  name: {sys.argv[3]}",
        "services:",
        "  app:",
        f"    image: {sys.argv[2]}",
        "    command:",
        "      - /bin/sh",
        "      - -c",
        '      - "exit 0"',
        '      - "' + ("x" * 15000) + '"',
        '      - "' + ("y" * 15000) + '"',
        '      - "' + ("z" * 15000) + '"',
        "",
    ]
)
Path(sys.argv[1]).write_text(manifest, encoding="utf-8")
PY
	fi
	if [[ $outcome == PASS ]]; then cp -- "$S5_CASE_MANIFEST" "$S5_CASE_RAW/manifest.yaml" || outcome=FAIL; fi
	if [[ $outcome == PASS ]]; then
		python3 - "$S5_CASE_PREFIX/var/lib/termux-stacks/state.db" \
			>"$S5_CASE_RAW/storage-limit.before" <<'PY' || outcome=FAIL
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
page_count = connection.execute("pragma page_count").fetchone()[0]
print(f"page_count\t{page_count}")
if page_count <= 0:
    raise SystemExit(1)
PY
	fi
	if [[ $outcome == PASS ]]; then
		S5_STORAGE_PAGE_LIMIT=$(awk -F '\t' '$1 == "page_count" { print $2 }' "$S5_CASE_RAW/storage-limit.before") || outcome=FAIL
		[[ $S5_STORAGE_PAGE_LIMIT =~ ^[1-9][0-9]*$ ]] || outcome=FAIL
	fi
	if [[ $outcome == PASS ]] && ! s5_start_daemon sqlite_full; then outcome=FAIL; fi
	if [[ $outcome == PASS ]]; then
		if s5_cli storage-full-up up "$S5_CASE_MANIFEST"; then
			outcome=FAIL
		elif ! grep -F '[state_store]' "$S5_CASE_RAW/storage-full-up.stderr" >/dev/null || \
			! grep -E 'database or disk is full|database is full' "$S5_CASE_RAW/storage-full-up.stderr" >/dev/null; then
			outcome=FAIL
		fi
	fi
	if [[ -n $S5_ACTIVE_DAEMON_PID ]] && ! s5_stop_daemon TERM; then outcome=FAIL; fi
	if [[ $outcome == PASS ]]; then
		python3 - "$S5_CASE_PREFIX/var/lib/termux-stacks/state.db" \
			>"$S5_CASE_RAW/storage-limit.after" <<'PY' || outcome=FAIL
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
integrity = connection.execute("pragma integrity_check").fetchone()[0]
stacks = connection.execute("select count(*) from stacks").fetchone()[0]
operations = connection.execute("select count(*) from operations").fetchone()[0]
print(f"integrity_check\t{integrity}")
print(f"stacks\t{stacks}")
print(f"operations\t{operations}")
if integrity != "ok" or stacks != 0 or operations != 0:
    raise SystemExit(1)
PY
	fi
	if [[ $outcome == PASS ]]; then cp -- "$S5_CASE_ROOT/small.yaml" "$S5_CASE_MANIFEST" || outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_start_daemon none; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_cli recovery-up up "$S5_CASE_MANIFEST"; then outcome=FAIL; fi
	if [[ $outcome == PASS ]]; then alias=$(s5_db_value "select alias from services limit 1") || outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_cli recovery-down down "$stack"; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_stop_daemon TERM; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_remove_stopped_alias "$alias" storage-recovery; then outcome=FAIL; fi
	s5_finish_case "$outcome"
	device_result fault.storage_full "$outcome" "$([[ $outcome == PASS ]] && echo 0 || echo 1)" \
		"SQLite full error rolled back; integrity remained ok; restored database completed a lifecycle" - -
	[[ $outcome == PASS ]]
}

s5_tree_case() {
	local mode=$1 stack=s5-tree-$1 alias stdout_log start_ns end_ns duration_ms outcome=PASS
	s5_new_case "tree-$mode" "$stack" || return 1
	python3 - "$S5_CASE_MANIFEST" "$S5_ARCHIVE_SNAPSHOT" "$stack" "$mode" <<'PY' || outcome=FAIL
import json
from pathlib import Path
import shlex
import sys

mode = sys.argv[4]
if mode == "cooperate":
    grandchild_trap = "trap 'echo grandchild-term; exit 0' TERM"
    child_trap = "trap 'echo child-term; exit 0' TERM"
    root_trap = "trap 'echo root-term; sleep 1; exit 0' TERM"
elif mode == "ignore":
    grandchild_trap = child_trap = root_trap = "trap '' TERM"
else:
    raise SystemExit(2)

grandchild = f"{grandchild_trap}; echo grandchild-ready; while :; do sleep 1; done"
child = (
    f"{child_trap}; /bin/sh -c {shlex.quote(grandchild)} & "
    "echo child-ready; while :; do sleep 1; done"
)
root = (
    f"{root_trap}; /bin/sh -c {shlex.quote(child)} & "
    "echo root-ready; while :; do sleep 1; done"
)
manifest = "\n".join(
    [
        "apiVersion: termux-stacks/v1alpha1",
        "kind: Stack",
        "metadata:",
        f"  name: {sys.argv[3]}",
        "services:",
        "  app:",
        f"    image: {sys.argv[2]}",
        "    command:",
        "      - /bin/sh",
        "      - -c",
        "      - " + json.dumps(root),
        "",
    ]
)
Path(sys.argv[1]).write_text(manifest, encoding="utf-8")
PY
	if [[ $outcome == PASS ]]; then cp -- "$S5_CASE_MANIFEST" "$S5_CASE_RAW/manifest.yaml" || outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_start_daemon none; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_cli up up "$S5_CASE_MANIFEST"; then outcome=FAIL; fi
	if [[ $outcome == PASS ]]; then
		alias=$(s5_db_value "select alias from services limit 1") || outcome=FAIL
		stdout_log=$(s5_db_value "select stdout_log_path from services limit 1") || outcome=FAIL
	fi
	for marker in root-ready child-ready grandchild-ready; do
		if [[ $outcome == PASS ]] && ! s5_wait_log_contains "$stdout_log" "$marker"; then outcome=FAIL; fi
	done
	if [[ $outcome == PASS ]]; then
		start_ns=$(date +%s%N)
		s5_cli down down "$stack" || outcome=FAIL
		end_ns=$(date +%s%N)
		duration_ms=$(((end_ns - start_ns) / 1000000))
		printf 'duration_ms\t%s\n' "$duration_ms" >"$S5_CASE_RAW/down-timing.tsv"
	fi
	if [[ $outcome == PASS && $mode == cooperate ]]; then
		for marker in root-term child-term grandchild-term; do
			s5_wait_log_contains "$stdout_log" "$marker" || outcome=FAIL
		done
	fi
	if [[ $outcome == PASS && $mode == ignore && $duration_ms -lt 1800 ]]; then outcome=FAIL; fi
	if [[ -n $S5_ACTIVE_DAEMON_PID ]] && ! s5_stop_daemon TERM; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_remove_stopped_alias "$alias" tree; then outcome=FAIL; fi
	s5_finish_case "$outcome"
	device_result "signals.tree.$mode" "$outcome" "$([[ $outcome == PASS ]] && echo 0 || echo 1)" \
		"three-role tree drained through exact engine session; mode=$mode" - -
	[[ $outcome == PASS ]]
}

s5_fault_case() {
	local checkpoint=$1 cycle=$2
	local label=${checkpoint//_/-}-$cycle stack=s5-${checkpoint//_/-}-$cycle
	local cli_pid cli_rc alias_before=- alias_after=- observed=unknown cleanup=preserved outcome=PASS
	s5_new_case "$label" "$stack" || return 1
	s5_continue_before "$checkpoint" || outcome=FAIL
	if [[ $outcome == PASS ]] && ! s5_start_daemon fault; then outcome=FAIL; fi

	if [[ $outcome == PASS && $checkpoint == during_down ]]; then
		if ! s5_cli up-before-down up "$S5_CASE_MANIFEST"; then outcome=FAIL; fi
	fi
	if [[ $outcome == PASS ]]; then
		if [[ $checkpoint == during_down ]]; then
			s5_cli_background interrupted-down down "$stack"
		else
			s5_cli_background interrupted-up up "$S5_CASE_MANIFEST"
		fi
		cli_pid=$S5_LAST_CLI_PID
		s5_wait_file "$S5_CASE_FAULT/$checkpoint.reached" || outcome=FAIL
	fi
	if [[ $outcome == PASS ]]; then
		s5_stop_daemon KILL || outcome=FAIL
	fi
	if [[ ${cli_pid:-} =~ ^[1-9][0-9]*$ ]]; then
		s5_wait_pid "$cli_pid" 200
		cli_rc=$?
		[[ $cli_rc -ne 124 ]] || outcome=FAIL
	fi

	if [[ $outcome == PASS ]] && ! s5_start_daemon none; then outcome=FAIL; fi
	if [[ $outcome == PASS ]] && ! s5_cli recovered status "$stack"; then outcome=FAIL; fi

	case $checkpoint in
		before_intent)
			if [[ $outcome == PASS ]]; then
				jq -e '.observed_state == "absent"' "$S5_CASE_RAW/recovered.stdout" >/dev/null || outcome=FAIL
			fi
			if [[ $outcome == PASS ]]; then
				count=$(s5_db_count "select count(*) from stacks") || outcome=FAIL
				[[ $count == 0 ]] || outcome=FAIL
			fi
			observed=absent
			cleanup=none
			;;
		after_intent)
			if [[ $outcome == PASS ]] && ! s5_assert_status "$S5_CASE_RAW/recovered.stdout" failed absent; then outcome=FAIL; fi
			if [[ $outcome == PASS ]]; then alias_before=$(s5_db_value "select alias from services limit 1") || outcome=FAIL; fi
			if [[ $outcome == PASS ]] && ! s5_cli retry up "$S5_CASE_MANIFEST"; then outcome=FAIL; fi
			if [[ $outcome == PASS ]]; then alias_after=$(s5_db_value "select alias from services limit 1") || outcome=FAIL; fi
			if [[ $outcome == PASS && $alias_after == "$alias_before" ]]; then outcome=FAIL; fi
			if [[ $outcome == PASS ]] && ! s5_cli retry-down down "$stack"; then outcome=FAIL; fi
			if [[ $outcome == PASS ]] && ! s5_remove_stopped_alias "$alias_after" retry; then outcome=FAIL; fi
			observed=failed-absent-retry-new-alias
			cleanup=exact-new-alias
			;;
		after_install)
			if [[ $outcome == PASS ]] && ! s5_assert_status "$S5_CASE_RAW/recovered.stdout" failed installed; then outcome=FAIL; fi
			if [[ $outcome == PASS ]]; then alias_before=$(s5_db_value "select alias from services limit 1") || outcome=FAIL; fi
			if [[ $outcome == PASS ]] && ! s5_cli retry up "$S5_CASE_MANIFEST"; then outcome=FAIL; fi
			if [[ $outcome == PASS ]]; then alias_after=$(s5_db_value "select alias from services limit 1") || outcome=FAIL; fi
			if [[ $outcome == PASS && $alias_after != "$alias_before" ]]; then outcome=FAIL; fi
			if [[ $outcome == PASS ]] && ! s5_cli retry-down down "$stack"; then outcome=FAIL; fi
			if [[ $outcome == PASS ]] && ! s5_remove_stopped_alias "$alias_after" retry; then outcome=FAIL; fi
			observed=failed-installed-retry-same-alias
			cleanup=exact-same-alias
			;;
		after_start | before_commit | during_down)
			if [[ $outcome == PASS ]] && ! s5_assert_status "$S5_CASE_RAW/recovered.stdout" unknown installed; then outcome=FAIL; fi
			if [[ $outcome == PASS ]]; then alias_before=$(s5_db_value "select alias from services limit 1") || outcome=FAIL; fi
			if [[ $outcome == PASS ]] && ! s5_manual_unknown_cleanup unknown; then outcome=FAIL; fi
			alias_after=$alias_before
			observed=unknown-no-automatic-effect
			cleanup=qualified-session-and-exact-alias
			;;
		*) outcome=FAIL ;;
	esac

	if [[ -n $S5_ACTIVE_DAEMON_PID ]] && ! s5_stop_daemon TERM; then outcome=FAIL; fi
	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
		"$label" "$checkpoint" "$cycle" "$checkpoint-recovery" "$observed" \
		"$alias_before" "$alias_after" "$cleanup" >>"$S5_FAULTS_FILE"
	s5_finish_case "$outcome"
	device_result "fault.$checkpoint.$cycle" "$outcome" \
		"$([[ $outcome == PASS ]] && echo 0 || echo 1)" \
		"$observed; cleanup=$cleanup" - -
	[[ $outcome == PASS ]]
}

s5_cleanup() {
	local cleanup_rc=0
	if [[ $S5_CLEANUP_STATE == done ]]; then return 0; fi
	S5_CLEANUP_STATE=running
	s5_install_deferred_signal_handlers
	if [[ -n $S5_ACTIVE_DAEMON_PID ]]; then
		if s5_daemon_identity_matches; then
			kill -TERM "$S5_ACTIVE_DAEMON_PID" 2>/dev/null || cleanup_rc=1
			s5_wait_pid "$S5_ACTIVE_DAEMON_PID" 200 >/dev/null 2>&1 || cleanup_rc=1
		else
			cleanup_rc=1
		fi
		S5_ACTIVE_DAEMON_PID=
	fi
	if ((cleanup_rc != 0)); then
		S5_PRESERVE_RUNTIME=1
		S5_CLEANUP_FAILURES=$((S5_CLEANUP_FAILURES + 1))
	fi
	S5_CLEANUP_STATE=done
	return "$cleanup_rc"
}

s5_on_exit() {
	local original_rc=$? finish_rc=0 final_rc
	s5_install_deferred_signal_handlers
	trap - EXIT
	s5_cleanup || original_rc=1
	if ((S5_CLEANUP_FAILURES > 0)); then
		device_result cleanup.objects FAIL 1 "cleanup was ambiguous; runtime preserved" - -
	else
		device_result cleanup.objects PASS 0 "all exact test targets drained" - -
	fi
	if ((S5_PRESERVE_RUNTIME)); then
		device_metadata preserved_runtime "$DEVICE_RUNTIME_DIR"
		DEVICE_RUNTIME_DIR=
	fi
	device_finish || finish_rc=1
	device_cleanup
	if ((S5_DEFERRED_SIGNAL != 0)); then final_rc=$S5_DEFERRED_SIGNAL
	elif ((original_rc != 0 || finish_rc != 0 || DEVICE_FAILURE_COUNT > 0)); then final_rc=1
	else final_rc=0
	fi
	exit "$final_rc"
}

trap s5_on_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

preflight_ok=1
if [[ -z $S5_REAL_PREFIX || $S5_REAL_PREFIX != /* || -z $S5_REAL_HOME ]]; then preflight_ok=0; fi
if [[ $binary != /* || ! -x $binary || -L $binary ]]; then preflight_ok=0; fi
if [[ $archive != /* || ! -f $archive || -L $archive ]]; then preflight_ok=0; fi
if [[ ! $archive_sha256 =~ ^[0-9a-f]{64}$ ]]; then preflight_ok=0; fi
for command_name in proot-distro proot python3 jq sha256sum sync; do
	command -v "$command_name" >/dev/null 2>&1 || preflight_ok=0
done
if [[ $(uname -m) != aarch64 ]]; then preflight_ok=0; fi
if ((preflight_ok)); then
	S5_BOOT_ID=$(< /proc/sys/kernel/random/boot_id) || preflight_ok=0
fi
if ((preflight_ok)); then
	cp -- "$archive" "$S5_ARCHIVE_SNAPSHOT" || preflight_ok=0
fi
if ((preflight_ok)); then
	bash "$FIXTURE_DIR/verify-oci.sh" "$S5_ARCHIVE_SNAPSHOT" "$archive_sha256" \
		>"$S5_ARCHIVE_REPORT" 2>"$DEVICE_STDIO_DIR/preflight.archive.stderr" || preflight_ok=0
fi
if ((preflight_ok)); then
	env TERMUX__PREFIX="$S5_REAL_PREFIX" TERMUX__HOME="$S5_REAL_HOME" \
		PD_PROOT_BIN="$S5_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		proot-distro list --quiet >"$S5_REAL_BEFORE" 2>"$DEVICE_STDIO_DIR/preflight.real-list.stderr" || preflight_ok=0
fi
if ((preflight_ok)); then
	device_metadata binary "$binary"
	device_metadata binary_sha256 "$(sha256sum "$binary" | awk '{print $1}')"
	device_metadata archive_sha256 "$archive_sha256"
	device_metadata unknown_cycles "$unknown_cycles"
	device_metadata architecture "$(uname -m)"
	device_metadata proot_distro "$(dpkg-query -W -f='${Version}' proot-distro 2>/dev/null || printf unknown)"
	device_metadata proot "$(dpkg-query -W -f='${Version}' proot 2>/dev/null || printf unknown)"
	device_result preflight PASS 0 "aarch64, tools, binary, and blessed OCI archive qualified" - -
else
	device_result preflight FAIL 1 "preflight or OCI qualification failed" - "stdout-stderr/preflight.archive.stderr"
	exit 1
fi

overall_ok=1
s5_normal_case || exit 1
s5_tree_case cooperate || exit 1
s5_tree_case ignore || exit 1
s5_storage_full_case || exit 1
s5_fault_case before_intent 1 || exit 1
s5_fault_case after_intent 1 || exit 1
s5_fault_case after_install 1 || exit 1
for ((cycle = 1; cycle <= unknown_cycles; cycle += 1)); do
	s5_fault_case after_start "$cycle" || exit 1
done
s5_fault_case before_commit 1 || exit 1
s5_fault_case during_down 1 || exit 1

env TERMUX__PREFIX="$S5_REAL_PREFIX" TERMUX__HOME="$S5_REAL_HOME" \
	PD_PROOT_BIN="$S5_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
	proot-distro list --quiet >"$S5_REAL_AFTER" 2>"$DEVICE_STDIO_DIR/postflight.real-list.stderr" || overall_ok=0
if cmp -s "$S5_REAL_BEFORE" "$S5_REAL_AFTER"; then
	device_result postflight.real-runtime PASS 0 "real container inventory is unchanged" - -
else
	device_result postflight.real-runtime FAIL 1 "real container inventory changed or could not be observed" - -
	overall_ok=0
	S5_PRESERVE_RUNTIME=1
fi

if ((overall_ok == 0)); then exit 1; fi
