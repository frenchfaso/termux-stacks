#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
FIXTURE_DIR=$SCRIPT_DIR/fixtures/s4

DEVICE_PHASE=S4
DEVICE_RUN_LABEL=termux-stacks-s4
DEVICE_RUNTIME_LABEL=txs-s4
DEVICE_HARNESS_VERSION=1
DEVICE_AUTOMATIC_SCOPE=$'The harness used one fresh private TERMUX__PREFIX and one random exact-name\nalias per attempt. It qualified a complete install, SIGKILL at a loopback\ndownload hold, and SIGSTOP+SIGKILL at the public second-layer barrier.\nOnly two positive exact public list observations establish owned; every other\npost-invocation outcome is ambiguous and preserves its sandbox.'

# shellcheck source=tests/device/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
	cat <<'EOF'
Usage:
  bash tests/device/s4.sh --archive ABSOLUTE_OCI_ARCHIVE \
    --archive-sha256 LOWERCASE_SHA256 [--output-root ABSOLUTE_DIR] \
    [--fault-cycles 1..10]

S4 mutates only fresh synthetic proot-distro runtimes. The default is three
download faults and three extraction faults. It removes an alias only after
two exact positive public observations prove ownership. Ambiguous attempts
fail closed and retain their private sandbox for manual review.
EOF
}

archive=
archive_sha256=
output_root=
fault_cycles=3
while (($# > 0)); do
	case $1 in
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
		--fault-cycles)
			[[ $# -ge 2 ]] || { device_error "--fault-cycles requires a value"; exit 2; }
			fault_cycles=$2
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

if [[ -z $archive || -z $archive_sha256 ]]; then
	usage >&2
	exit 2
fi
if [[ ! $fault_cycles =~ ^([1-9]|10)$ ]]; then
	device_error "--fault-cycles must be an integer from 1 through 10"
	exit 2
fi
fault_cycles=$((10#$fault_cycles))

device_init "$output_root" || exit $?

S4_RAW_DIR=$DEVICE_EVIDENCE_DIR/install
S4_GOLDEN_FILE=$DEVICE_EVIDENCE_DIR/golden.tsv
S4_INTENT_FILE=$DEVICE_EVIDENCE_DIR/intent.tsv
S4_ORACLE_FILE=$DEVICE_EVIDENCE_DIR/oracle.tsv
S4_CLEANUP_FILE=$DEVICE_EVIDENCE_DIR/cleanup.raw
S4_PRESERVED_FILE=$DEVICE_EVIDENCE_DIR/preserved.tsv
S4_REAL_FINAL=$DEVICE_EVIDENCE_DIR/real-containers.final
mkdir -m 0700 -- "$S4_RAW_DIR"
printf 'attempt\talias\tcase\tcycle\tbarrier\texpected\tobserved\tinvoked\tbarrier_seen\tinstall_exit\tlist1_rc\tlist1_exact\tlist2_rc\tlist2_exact\tcleanup\n' >"$S4_GOLDEN_FILE"
printf 'time_utc\taction\ttarget\n' >"$S4_INTENT_FILE"
printf 'phase\tkind\tpid\tstarttime\tppid\tpgid\tsid\tstate\tcomm\tboot_id\n' >"$S4_ORACLE_FILE"
printf 'attempt\talias\tsandbox\treason\n' >"$S4_PRESERVED_FILE"
: >"$S4_CLEANUP_FILE"

run_id=$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')
S4_REAL_PREFIX=${PREFIX:-}
S4_BOOT_ID=
S4_CLEANUP_STATE=pending
S4_CLEANUP_FAILURES=0
S4_DEFERRED_SIGNAL=0
S4_LAST_PID=
S4_LAST_WAIT_RC=-
S4_INSTALL_WAIT_RC=-
S4_SERVER_PORT=
S4_CLASSIFICATION=ambiguous
S4_LIST1_RC=-
S4_LIST2_RC=-
S4_LIST1_EXACT=-
S4_LIST2_EXACT=-
S4_ATTEMPT=
S4_CASE=
S4_CYCLE=
S4_ALIAS=
S4_SANDBOX=
S4_PREFIX=
S4_HOME=
S4_CONTROL=
S4_SENTINEL=
S4_SENTINEL_VALUE=
S4_SANDBOX_ID=
S4_INVOKED=0
S4_BARRIER_SEEN=0
S4_ATTEMPT_CLEANUP=preserved

readonly S4_INSTALL_TTL=300
readonly S4_SERVER_TTL=95

declare -a S4_PIDS=()
declare -a S4_ALIASES=()
declare -A S4_ALIAS_SEEN=()
declare -A S4_PROC_KIND=()
declare -A S4_PROC_ACTIVE=()
declare -A S4_PROC_QUALIFIED=()
declare -A S4_PROC_STARTTIME=()
declare -A S4_PROC_PGID=()
declare -A S4_PROC_SID=()
declare -A S4_PROC_WAIT_RC=()
declare -A S4_PROC_REAPED=()
declare -A S4_PROC_TTL=()
declare -A S4_PRESERVED=()

s4_defer_signal() {
	local code=$1
	if ((S4_DEFERRED_SIGNAL == 0)); then S4_DEFERRED_SIGNAL=$code; fi
}

s4_install_deferred_signal_handlers() {
	trap 's4_defer_signal 129' HUP
	trap 's4_defer_signal 130' INT
	trap 's4_defer_signal 143' TERM
}

s4_intent() {
	local action=$1 target=$2
	printf '%s\t%s\t%s\n' \
		"$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
		"$(device_sanitize_tsv "$action")" \
		"$(device_sanitize_tsv "$target")" >>"$S4_INTENT_FILE" || return 1
	sync -f "$S4_INTENT_FILE"
}

s4_proc_fields() {
	local pid=$1 line rest
	local -a fields
	[[ $pid =~ ^[1-9][0-9]*$ && -r /proc/$pid/stat ]] || return 1
	IFS= read -r line <"/proc/$pid/stat" || return 1
	[[ $line == *') '* ]] || return 1
	rest=${line##*) }
	read -r -a fields <<<"$rest"
	((${#fields[@]} >= 20)) || return 1
	printf '%s\t%s\t%s\t%s\t%s\n' \
		"${fields[19]}" "${fields[1]}" "${fields[2]}" "${fields[3]}" "${fields[0]}"
}

s4_register_scope() {
	local pid=$1 kind=$2 phase=$3
	local fields starttime ppid pgid sid state comm snapshot iteration
	for ((iteration = 0; iteration < 50; iteration += 1)); do
		fields=$(s4_proc_fields "$pid") || return 1
		IFS=$'\t' read -r starttime ppid pgid sid state <<<"$fields"
		if [[ $pgid == "$pid" && $sid == "$pid" && \
			$state != Z && $state != X && $state != x ]]; then
			break
		fi
		sleep 0.02
	done
	[[ $pgid == "$pid" && $sid == "$pid" && \
		$state != Z && $state != X && $state != x ]] || return 1
	IFS= read -r comm <"/proc/$pid/comm" || return 1
	snapshot=$S4_RAW_DIR/$phase.scope-calibration
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 1
	awk -v wanted_pid="$pid" -v wanted_pgid="$pgid" -v wanted_sid="$sid" '
		$1 == wanted_pid && $2 == wanted_pgid && $3 == wanted_sid { found = 1 }
		END { exit found ? 0 : 1 }
	' "$snapshot" || return 1
	S4_PROC_STARTTIME[$pid]=$starttime
	S4_PROC_PGID[$pid]=$pgid
	S4_PROC_SID[$pid]=$sid
	S4_PROC_QUALIFIED[$pid]=1
	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
		"$phase" "$kind" "$pid" "$starttime" "$ppid" "$pgid" "$sid" \
		"$state" "$comm" "$S4_BOOT_ID" >>"$S4_ORACLE_FILE"
}

s4_identity_matches() {
	local pid=$1 fields starttime _ppid pgid sid state boot_now
	[[ ${S4_PROC_QUALIFIED[$pid]:-0} == 1 ]] || return 2
	boot_now=$(< /proc/sys/kernel/random/boot_id) || return 2
	[[ $boot_now == "$S4_BOOT_ID" ]] || return 2
	fields=$(s4_proc_fields "$pid") || return 1
	IFS=$'\t' read -r starttime _ppid pgid sid state <<<"$fields"
	[[ $starttime == "${S4_PROC_STARTTIME[$pid]}" && \
		$pgid == "${S4_PROC_PGID[$pid]}" && $sid == "${S4_PROC_SID[$pid]}" && \
		$state != Z && $state != X && $state != x ]]
}

s4_scope_authorized() {
	local pid=$1 phase=$2 pgid sid snapshot
	pgid=${S4_PROC_PGID[$pid]:-}
	sid=${S4_PROC_SID[$pid]:-}
	[[ $pid != "$$" && $pgid == "$pid" && $sid == "$pid" ]] || return 1
	s4_identity_matches "$pid" || return 1
	snapshot=$S4_RAW_DIR/$phase.scope-authorize
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 1
	awk -v leader="$pid" -v group="$pgid" -v session="$sid" '
		$2 == group { count += 1; if ($3 != session) bad = 1 }
		$3 == session && $2 != group { bad = 1 }
		$1 == leader && $2 == group && $3 == session { have_leader = 1 }
		END { exit (count > 0 && have_leader && !bad) ? 0 : 1 }
	' "$snapshot"
}

s4_scope_empty() {
	local pid=$1 phase=$2 pgid sid snapshot scan_rc
	pgid=${S4_PROC_PGID[$pid]:-}
	sid=${S4_PROC_SID[$pid]:-}
	[[ $pgid =~ ^[1-9][0-9]*$ && $sid =~ ^[1-9][0-9]*$ ]] || return 2
	snapshot=$S4_RAW_DIR/$phase.scope-current
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 2
	awk -v group="$pgid" -v session="$sid" '
		$2 == group || $3 == session { found = 1 }
		END { exit found ? 0 : 1 }
	' "$snapshot"
	scan_rc=$?
	case $scan_rc in
		0) return 1 ;;
		1) return 0 ;;
		*) return 2 ;;
	esac
}

s4_scope_stopped() {
	local pid=$1 phase=$2 pgid sid snapshot
	pgid=${S4_PROC_PGID[$pid]:-}
	sid=${S4_PROC_SID[$pid]:-}
	[[ $pgid == "$pid" && $sid == "$pid" ]] || return 1
	snapshot=$S4_RAW_DIR/$phase.scope-stopped
	ps -A -o pid=,pgid=,sid=,stat=,comm= >"$snapshot" 2>&1 || return 1
	awk -v leader="$pid" -v group="$pgid" -v session="$sid" '
		$2 == group || $3 == session {
			count += 1
			state = substr($4, 1, 1)
			if ($2 != group || $3 != session || state !~ /^[TtZXx]$/) bad = 1
			if ($1 == leader && state ~ /^[Tt]$/) leader_stopped = 1
			if ($1 != leader && state ~ /^[Tt]$/) child_stopped = 1
		}
		END {
			exit (!bad && count >= 2 && leader_stopped && child_stopped) ? 0 : 1
		}
	' "$snapshot"
}

s4_no_later_public_phase() {
	local install_stderr=$S4_RAW_DIR/$S4_ATTEMPT.install.stderr
	grep -F -- 'Applying layer 2/2' "$install_stderr" >/dev/null 2>&1 || return 1
	if grep -F -e 'Finished installation.' -e "Updating '/etc/" \
		-e 'Registering Android-specific' -e 'manifest.json' \
		"$install_stderr" >/dev/null 2>&1; then
		return 1
	fi
}

s4_reap_if_dead() {
	local pid=$1 fields _start _ppid _pgid _sid state wait_rc=0
	if [[ ${S4_PROC_REAPED[$pid]:-0} == 1 ]]; then return 0; fi
	if [[ ! -e /proc/$pid/stat ]]; then
		wait "$pid" 2>/dev/null || wait_rc=$?
		S4_PROC_WAIT_RC[$pid]=$wait_rc
		S4_PROC_REAPED[$pid]=1
		return 0
	fi
	if ! fields=$(s4_proc_fields "$pid"); then
		if [[ ! -e /proc/$pid/stat ]]; then
			wait "$pid" 2>/dev/null || wait_rc=$?
			S4_PROC_WAIT_RC[$pid]=$wait_rc
			S4_PROC_REAPED[$pid]=1
			return 0
		fi
		return 2
	fi
	IFS=$'\t' read -r _start _ppid _pgid _sid state <<<"$fields"
	if [[ $state == Z || $state == X || $state == x ]]; then
		wait "$pid" 2>/dev/null || wait_rc=$?
		S4_PROC_WAIT_RC[$pid]=$wait_rc
		S4_PROC_REAPED[$pid]=1
		return 0
	fi
	return 1
}

s4_wait_process() {
	local pid=$1 phase=$2 seconds=${3:-180}
	local iteration max_iterations=$((seconds * 10)) reaped=0 empty_rc
	for ((iteration = 0; iteration < max_iterations; iteration += 1)); do
		if ((reaped == 0)); then
			s4_reap_if_dead "$pid"
			case $? in
				0) reaped=1 ;;
				1) sleep 0.1; continue ;;
				*) return 1 ;;
			esac
		fi
		s4_scope_empty "$pid" "$phase"
		empty_rc=$?
		case $empty_rc in
			0)
				S4_LAST_WAIT_RC=${S4_PROC_WAIT_RC[$pid]:--}
				S4_PROC_ACTIVE[$pid]=0
				return 0
				;;
			1) sleep 0.1 ;;
			*) return 1 ;;
		esac
	done
	return 1
}

s4_wait_unqualified_process() {
	local pid=$1 seconds=$2 iteration fields _start _ppid _pgid _sid state wait_rc=0
	for ((iteration = 0; iteration < seconds * 10; iteration += 1)); do
		if [[ ${S4_PROC_REAPED[$pid]:-0} == 1 ]]; then
			S4_PROC_ACTIVE[$pid]=0
			return 0
		fi
		if [[ ! -e /proc/$pid/stat ]]; then
			wait "$pid" 2>/dev/null || wait_rc=$?
			S4_PROC_WAIT_RC[$pid]=$wait_rc
			S4_PROC_REAPED[$pid]=1
			S4_PROC_ACTIVE[$pid]=0
			return 0
		fi
		fields=$(s4_proc_fields "$pid") || return 1
		IFS=$'\t' read -r _start _ppid _pgid _sid state <<<"$fields"
		if [[ $state == Z || $state == X || $state == x ]]; then
			wait "$pid" 2>/dev/null || wait_rc=$?
			S4_PROC_WAIT_RC[$pid]=$wait_rc
			S4_PROC_REAPED[$pid]=1
			S4_PROC_ACTIVE[$pid]=0
			return 0
		fi
		sleep 0.1
	done
	return 1
}

s4_signal_scope() {
	local pid=$1 signal=$2 phase=$3 pgid
	s4_scope_authorized "$pid" "$phase" || return 1
	pgid=${S4_PROC_PGID[$pid]}
	s4_intent "signal-${signal,,}" "${S4_PROC_KIND[$pid]}:$pid:$pgid" || return 1
	kill -"$signal" -- "-$pgid" 2>"$S4_RAW_DIR/$phase.signal.stderr"
}

s4_stop_at_layer_marker_then_kill() {
	local pid=$1 phase=$2 pgid iteration
	s4_scope_authorized "$pid" "$phase" || return 1
	pgid=${S4_PROC_PGID[$pid]}
	s4_intent signal-stop "${S4_PROC_KIND[$pid]}:$pid:$pgid" || return 1
	kill -STOP -- "-$pgid" 2>"$S4_RAW_DIR/$phase.stop.stderr" || return 1
	for ((iteration = 0; iteration < 50; iteration += 1)); do
		if s4_scope_stopped "$pid" "$phase"; then break; fi
		s4_identity_matches "$pid" || return 1
		sleep 0.02
	done
	s4_scope_stopped "$pid" "$phase" || return 1
	s4_identity_matches "$pid" || return 1
	s4_no_later_public_phase || return 1
	s4_intent signal-kill "${S4_PROC_KIND[$pid]}:$pid:$pgid" || return 1
	kill -KILL -- "-$pgid" 2>"$S4_RAW_DIR/$phase.kill.stderr"
}

s4_public_list() {
	local prefix=$1 home=$2 target=$3
	timeout --signal=KILL 15 env \
		TERMUX__PREFIX="$prefix" TERMUX__HOME="$home" \
		PD_PROOT_BIN="$S4_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		COLUMNS=240 proot-distro list --quiet >"$target" 2>"$target.stderr"
}

s4_public_remove() {
	local prefix=$1 home=$2 alias=$3 target=$4
	timeout --signal=KILL 180 env \
		TERMUX__PREFIX="$prefix" TERMUX__HOME="$home" \
		PD_PROOT_BIN="$S4_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		COLUMNS=240 proot-distro remove --quiet "$alias" >"$target" 2>"$target.stderr"
}

s4_wait_pattern() {
	local file=$1 pattern=$2 pid=$3 seconds=$4 iteration
	for ((iteration = 0; iteration < seconds * 10; iteration += 1)); do
		if [[ -f $file ]] && grep -F -- "$pattern" "$file" >/dev/null 2>&1; then return 0; fi
		s4_identity_matches "$pid" || return 1
		sleep 0.1
	done
	return 1
}

s4_launch_install() {
	local source=$1 phase=$2 pid
	s4_intent invoke-install "$S4_ATTEMPT:$S4_ALIAS:$source" || return 1
	setsid timeout --foreground --signal=KILL "$S4_INSTALL_TTL" env \
		TERMUX__PREFIX="$S4_PREFIX" TERMUX__HOME="$S4_HOME" \
		PD_PROOT_BIN="$S4_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		PYTHONUNBUFFERED=1 COLUMNS=240 proot-distro install --architecture aarch64 \
		--name "$S4_ALIAS" "$source" \
		>"$S4_RAW_DIR/$S4_ATTEMPT.install.stdout" \
		2>"$S4_RAW_DIR/$S4_ATTEMPT.install.stderr" &
	pid=$!
	S4_PIDS+=("$pid")
	S4_PROC_KIND[$pid]=install
	S4_PROC_ACTIVE[$pid]=1
	S4_PROC_QUALIFIED[$pid]=0
	S4_PROC_REAPED[$pid]=0
	S4_PROC_TTL[$pid]=$S4_INSTALL_TTL
	S4_INVOKED=1
	s4_register_scope "$pid" install "$phase" || return 1
	S4_LAST_PID=$pid
}

s4_launch_server() {
	local phase=$1 ready=$S4_CONTROL/server.ready events=$S4_CONTROL/server.events
	local release=$S4_CONTROL/server.release pid iteration schema port
	s4_intent launch-loopback-server "$S4_ATTEMPT:$archive" || return 1
	setsid python "$FIXTURE_DIR/slow_http.py" \
		--archive "$archive" --ready "$ready" --events "$events" --release "$release" \
		>"$S4_RAW_DIR/$S4_ATTEMPT.server.stdout" \
		2>"$S4_RAW_DIR/$S4_ATTEMPT.server.stderr" &
	pid=$!
	S4_PIDS+=("$pid")
	S4_PROC_KIND[$pid]=loopback-server
	S4_PROC_ACTIVE[$pid]=1
	S4_PROC_QUALIFIED[$pid]=0
	S4_PROC_REAPED[$pid]=0
	S4_PROC_TTL[$pid]=$S4_SERVER_TTL
	s4_register_scope "$pid" loopback-server "$phase" || return 1
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		[[ -f $ready ]] && break
		s4_identity_matches "$pid" || return 1
		sleep 0.1
	done
	[[ -f $ready && ! -L $ready ]] || return 1
	IFS=$'\t' read -r schema port <"$ready" || return 1
	[[ $schema == v1 && $port =~ ^[1-9][0-9]{0,4}$ ]] || return 1
	((port <= 65535)) || return 1
	S4_LAST_PID=$pid
	S4_SERVER_PORT=$port
}

s4_server_is_holding() {
	local pid=$1 events=$S4_CONTROL/server.events release=$S4_CONTROL/server.release
	s4_identity_matches "$pid" || return 1
	[[ -f $events && ! -L $events && ! -e $release && ! -L $release ]] || return 1
	awk -F '\t' '
		NF != 3 { bad = 1 }
		$2 == "listening" { listening += 1 }
		$2 == "get" { get += 1 }
		$2 == "barrier" { barrier += 1 }
		$2 == "released" || $2 == "server_timeout" || $2 == "complete" ||
		$2 == "client_disconnected" || $2 == "io_error" { bad = 1 }
		{ last = $2 }
		END {
			exit (!bad && listening == 1 && get == 1 && barrier == 1 &&
				last == "barrier") ? 0 : 1
		}
	' "$events"
}

s4_server_released_after_disconnect() {
	local events=$S4_CONTROL/server.events
	[[ -f $events && ! -L $events ]] || return 1
	awk -F '\t' '
		NF != 3 { bad = 1 }
		$2 == "released" { released += 1 }
		$2 == "client_disconnected" || $2 == "io_error" { disconnected += 1 }
		$2 == "server_timeout" || $2 == "complete" { bad = 1 }
		{ last = $2 }
		END {
			exit (!bad && released == 1 && disconnected == 1 &&
				(last == "client_disconnected" || last == "io_error")) ? 0 : 1
		}
	' "$events"
}

s4_release_server() {
	local pid=$1 phase=$2 release=$S4_CONTROL/server.release
	if [[ ${S4_PROC_ACTIVE[$pid]:-0} != 1 ]]; then return 0; fi
	s4_intent release-loopback-server "$S4_ATTEMPT:$pid" || return 1
	printf '%s\n' release >"$release" || return 1
	sync -f "$release" || return 1
	if s4_wait_process "$pid" "$phase" 20; then
		[[ $S4_LAST_WAIT_RC == 0 ]]
		return
	fi
	if s4_signal_scope "$pid" KILL "$phase-timeout"; then
		s4_wait_process "$pid" "$phase-killed" 20 || true
	fi
	return 1
}

s4_capture_server_evidence() {
	local name source target
	for name in ready events; do
		source=$S4_CONTROL/server.$name
		target=$S4_RAW_DIR/$S4_ATTEMPT.server.$name
		[[ -f $source && ! -L $source ]] || return 1
		cp -- "$source" "$target" || return 1
		chmod 0600 "$target" || return 1
	done
	if [[ -f $S4_CONTROL/server.release && ! -L $S4_CONTROL/server.release ]]; then
		cp -- "$S4_CONTROL/server.release" \
			"$S4_RAW_DIR/$S4_ATTEMPT.server.release" || return 1
		chmod 0600 "$S4_RAW_DIR/$S4_ATTEMPT.server.release" || return 1
	fi
}

s4_attempt_identity_matches() {
	local canonical id_now
	[[ -n $S4_SANDBOX && -d $S4_SANDBOX && ! -L $S4_SANDBOX ]] || return 1
	canonical=$(realpath -e -- "$S4_SANDBOX" 2>/dev/null) || return 1
	id_now=$(stat -c '%d:%i' -- "$S4_SANDBOX" 2>/dev/null) || return 1
	[[ $canonical == "$S4_SANDBOX" && $id_now == "$S4_SANDBOX_ID" && \
		-f $S4_SENTINEL && ! -L $S4_SENTINEL && \
		$(<"$S4_SENTINEL") == "$S4_SENTINEL_VALUE" ]]
}

s4_preserve_attempt() {
	local reason=$1
	if [[ ${S4_PRESERVED[$S4_ATTEMPT]:-0} == 0 ]]; then
		printf '%s\t%s\t%s\t%s\n' "$S4_ATTEMPT" "$S4_ALIAS" "$S4_SANDBOX" \
			"$(device_sanitize_tsv "$reason")" >>"$S4_PRESERVED_FILE"
		S4_PRESERVED[$S4_ATTEMPT]=1
	fi
	S4_ATTEMPT_CLEANUP=preserved
}

s4_prepare_attempt() {
	local case_name=$1 cycle=$2 alias_candidate attempt_tag real_target synthetic_target
	local alias_random sandbox_random sentinel_random
	S4_CASE=$case_name
	S4_CYCLE=$cycle
	attempt_tag=${case_name,,}-$cycle
	S4_ATTEMPT=${case_name^^}-$cycle
	while :; do
		alias_random=$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n') || return 1
		alias_candidate=txs-s4-$run_id-$attempt_tag-$alias_random
		if [[ ! ${S4_ALIAS_SEEN[$alias_candidate]+set} ]]; then break; fi
	done
	S4_ALIAS=$alias_candidate
	S4_ALIAS_SEEN[$S4_ALIAS]=1
	S4_ALIASES+=("$S4_ALIAS")
	sandbox_random=$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n') || return 1
	S4_SANDBOX=$DEVICE_RUN_DIR/sandboxes/$attempt_tag-$sandbox_random
	S4_PREFIX=$S4_SANDBOX/prefix
	S4_HOME=$S4_SANDBOX/home
	S4_CONTROL=$S4_SANDBOX/control
	S4_SENTINEL=$S4_SANDBOX/.termux-stacks-s4-sentinel
	sentinel_random=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n') || return 1
	S4_SENTINEL_VALUE=$run_id-$sentinel_random
	S4_SANDBOX_ID=
	S4_INVOKED=0
	S4_BARRIER_SEEN=0
	S4_ATTEMPT_CLEANUP=preserved
	S4_CLASSIFICATION=ambiguous
	S4_LIST1_RC=-
	S4_LIST2_RC=-
	S4_LIST1_EXACT=-
	S4_LIST2_EXACT=-
	S4_LAST_WAIT_RC=-
	S4_INSTALL_WAIT_RC=-
	S4_SERVER_PORT=

	s4_intent prepare-attempt "$S4_ATTEMPT:$S4_ALIAS:$S4_SANDBOX" || return 1
	mkdir -m 0700 -- "$S4_SANDBOX" "$S4_PREFIX" "$S4_HOME" "$S4_CONTROL" || return 1
	printf '%s\n' "$S4_SENTINEL_VALUE" >"$S4_SENTINEL" || return 1
	sync -f "$S4_SENTINEL" || return 1
	S4_SANDBOX=$(realpath -e -- "$S4_SANDBOX") || return 1
	S4_PREFIX=$S4_SANDBOX/prefix
	S4_HOME=$S4_SANDBOX/home
	S4_CONTROL=$S4_SANDBOX/control
	S4_SENTINEL=$S4_SANDBOX/.termux-stacks-s4-sentinel
	S4_SANDBOX_ID=$(stat -c '%d:%i' -- "$S4_SANDBOX") || return 1

	real_target=$S4_RAW_DIR/$S4_ATTEMPT.real-pre
	timeout --signal=KILL 15 env PD_FORCE_NO_COLORS=true COLUMNS=240 \
		proot-distro list --quiet >"$real_target" 2>"$real_target.stderr" || return 1
	[[ ! -s $real_target.stderr ]] || return 1
	if grep -Fx -- "$S4_ALIAS" "$real_target" >/dev/null 2>&1; then return 1; fi

	device_capture_timed 15 "$S4_ATTEMPT.synthetic-help" env \
		TERMUX__PREFIX="$S4_PREFIX" TERMUX__HOME="$S4_HOME" \
		PD_PROOT_BIN="$S4_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		proot-distro help
	if ((DEVICE_CAPTURE_RC != 0)) || ! grep -F -- "$S4_PREFIX/var/lib/proot-distro" \
		"$DEVICE_CAPTURE_STDOUT" "$DEVICE_CAPTURE_STDERR" >/dev/null; then
		return 1
	fi

	synthetic_target=$S4_RAW_DIR/$S4_ATTEMPT.synthetic-pre
	s4_public_list "$S4_PREFIX" "$S4_HOME" "$synthetic_target" || return 1
	[[ ! -s $synthetic_target && ! -s $synthetic_target.stderr ]]
}

s4_observe_alias() {
	local first=$S4_RAW_DIR/$S4_ATTEMPT.list-1 second=$S4_RAW_DIR/$S4_ATTEMPT.list-2
	local first_total second_total
	S4_CLASSIFICATION=ambiguous
	s4_public_list "$S4_PREFIX" "$S4_HOME" "$first"
	S4_LIST1_RC=$?
	S4_LIST1_EXACT=$(awk -v alias="$S4_ALIAS" '$0 == alias { count += 1 } END { print count + 0 }' "$first")
	first_total=$(awk 'NF { count += 1 } END { print count + 0 }' "$first")
	sleep 0.2
	s4_public_list "$S4_PREFIX" "$S4_HOME" "$second"
	S4_LIST2_RC=$?
	S4_LIST2_EXACT=$(awk -v alias="$S4_ALIAS" '$0 == alias { count += 1 } END { print count + 0 }' "$second")
	second_total=$(awk 'NF { count += 1 } END { print count + 0 }' "$second")
	if [[ $S4_LIST1_RC == 0 && $S4_LIST2_RC == 0 && \
		$S4_LIST1_EXACT == 1 && $S4_LIST2_EXACT == 1 && \
		$first_total == 1 && $second_total == 1 && \
		! -s $first.stderr && ! -s $second.stderr ]]; then
		S4_CLASSIFICATION=owned
	fi
}

s4_cleanup_owned_attempt() {
	local remove_target=$S4_RAW_DIR/$S4_ATTEMPT.remove
	local post_first=$S4_RAW_DIR/$S4_ATTEMPT.post-remove-1
	local post_second=$S4_RAW_DIR/$S4_ATTEMPT.post-remove-2
	[[ $S4_CLASSIFICATION == owned ]] || return 1
	s4_attempt_identity_matches || return 1
	s4_intent remove-owned-alias "$S4_ATTEMPT:$S4_ALIAS" || return 1
	s4_public_remove "$S4_PREFIX" "$S4_HOME" "$S4_ALIAS" "$remove_target" || return 1
	s4_public_list "$S4_PREFIX" "$S4_HOME" "$post_first" || return 1
	sleep 0.2
	s4_public_list "$S4_PREFIX" "$S4_HOME" "$post_second" || return 1
	[[ ! -s $post_first && ! -s $post_first.stderr && \
		! -s $post_second && ! -s $post_second.stderr ]] || return 1
	s4_attempt_identity_matches || return 1
	s4_intent remove-owned-sandbox "$S4_ATTEMPT:$S4_SANDBOX" || return 1
	rm -rf -- "$S4_SANDBOX" || return 1
	[[ ! -e $S4_SANDBOX && ! -L $S4_SANDBOX ]] || return 1
	S4_ATTEMPT_CLEANUP=removed
}

s4_record_golden() {
	local barrier=$1 expected=${2:-owned}
	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
		"$S4_ATTEMPT" "$S4_ALIAS" "$S4_CASE" "$S4_CYCLE" "$barrier" "$expected" \
		"$S4_CLASSIFICATION" "$S4_INVOKED" "$S4_BARRIER_SEEN" "$S4_INSTALL_WAIT_RC" \
		"$S4_LIST1_RC" "$S4_LIST1_EXACT" "$S4_LIST2_RC" "$S4_LIST2_EXACT" \
		"$S4_ATTEMPT_CLEANUP" >>"$S4_GOLDEN_FILE"
}

s4_result() {
	local id=$1 ok=$2 detail=$3
	if ((ok)); then
		device_result "$id" PASS 0 "$detail" - -
	else
		device_result "$id" FAIL 1 "$detail" - -
	fi
}

s4_cleanup_active_processes() {
	local pid ttl
	for pid in "${S4_PIDS[@]}"; do
		[[ ${S4_PROC_ACTIVE[$pid]:-0} == 1 ]] || continue
		if [[ ${S4_PROC_QUALIFIED[$pid]:-0} != 1 ]]; then
			s4_register_scope "$pid" "${S4_PROC_KIND[$pid]:-unknown}" \
				"cleanup-$pid-late-qualification" || true
		fi
		if [[ ${S4_PROC_QUALIFIED[$pid]:-0} != 1 ]]; then
			ttl=${S4_PROC_TTL[$pid]:-$S4_INSTALL_TTL}
			if s4_wait_unqualified_process "$pid" "$((ttl + 10))"; then
				printf 'DRAINED_AFTER_TTL_UNQUALIFIED\t%s\t%s\n' \
					"$pid" "${S4_PROC_KIND[$pid]:-unknown}" >>"$S4_CLEANUP_FILE"
			else
				printf 'LIVE_AFTER_TTL_UNQUALIFIED\t%s\t%s\n' \
					"$pid" "${S4_PROC_KIND[$pid]:-unknown}" >>"$S4_CLEANUP_FILE"
			fi
			S4_CLEANUP_FAILURES=$((S4_CLEANUP_FAILURES + 1))
			continue
		fi
		if s4_wait_process "$pid" "cleanup-$pid-natural" 1; then
			printf 'DRAINED_NATURALLY\t%s\t%s\n' "$pid" "${S4_PROC_KIND[$pid]}" \
				>>"$S4_CLEANUP_FILE"
			continue
		fi
		if s4_signal_scope "$pid" KILL "cleanup-$pid" && \
			s4_wait_process "$pid" "cleanup-$pid-drain" 30; then
			printf 'DRAINED\t%s\t%s\n' "$pid" "${S4_PROC_KIND[$pid]}" >>"$S4_CLEANUP_FILE"
			continue
		fi
		S4_CLEANUP_FAILURES=$((S4_CLEANUP_FAILURES + 1))
	done
}

s4_cleanup() {
	local alias
	[[ $S4_CLEANUP_STATE == pending ]] || return 0
	s4_install_deferred_signal_handlers
	S4_CLEANUP_STATE=running
	printf 'cleanup_started\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$S4_CLEANUP_FILE"
	s4_cleanup_active_processes
	if ! timeout --signal=KILL 15 env PD_FORCE_NO_COLORS=true COLUMNS=240 \
		proot-distro list --quiet >"$S4_REAL_FINAL" 2>"$S4_REAL_FINAL.stderr"; then
		S4_CLEANUP_FAILURES=$((S4_CLEANUP_FAILURES + 1))
	elif [[ -s $S4_REAL_FINAL.stderr ]]; then
		printf 'REAL_RUNTIME_STDERR\tpublic inventory emitted diagnostics\n' >>"$S4_CLEANUP_FILE"
		S4_CLEANUP_FAILURES=$((S4_CLEANUP_FAILURES + 1))
	else
		for alias in "${S4_ALIASES[@]}"; do
			if grep -Fx -- "$alias" "$S4_REAL_FINAL" >/dev/null 2>&1; then
				printf 'REAL_RUNTIME_COLLISION\t%s\n' "$alias" >>"$S4_CLEANUP_FILE"
				S4_CLEANUP_FAILURES=$((S4_CLEANUP_FAILURES + 1))
			fi
		done
	fi
	printf 'cleanup_finished\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$S4_CLEANUP_FILE"
	S4_CLEANUP_STATE=done
}

s4_on_exit() {
	local original_rc=$1 was_pending=0
	s4_install_deferred_signal_handlers
	trap - EXIT
	[[ $S4_CLEANUP_STATE == pending ]] && was_pending=1
	s4_cleanup
	if ((was_pending)) && ((DEVICE_FINISHED == 0)); then
		if ((S4_CLEANUP_FAILURES == 0)); then
			device_result cleanup.processes PASS 0 \
				"all qualified child scopes drained; ambiguous sandboxes intentionally preserved" - -
		else
			device_result cleanup.processes FAIL 1 "one or more child scopes could not be drained" - -
		fi
	fi
	if ((DEVICE_FINISHED == 0)); then device_finish || true; fi
	device_cleanup
	if ((S4_CLEANUP_FAILURES > 0 || DEVICE_FAILURE_COUNT > 0)) && ((original_rc == 0)); then
		original_rc=1
	fi
	if ((S4_DEFERRED_SIGNAL > 0)); then original_rc=$S4_DEFERRED_SIGNAL; fi
	trap - HUP INT TERM
	exit "$original_rc"
}

trap 's4_on_exit $?' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

preflight_failed=0
for command_name in bash proot-distro proot timeout sha256sum tar gzip jq setsid ps \
	stat realpath dpkg-query awk grep find cp python date sync od tr; do
	if ! command -v "$command_name" >/dev/null 2>&1; then
		device_result "preflight.$command_name" FAIL 127 "$command_name is required" - -
		preflight_failed=1
	fi
done
if [[ $archive != /* || ! -f $archive ]]; then
	device_result preflight.archive FAIL 2 "archive must be an absolute regular file" - -
	preflight_failed=1
fi
if [[ ! $archive_sha256 =~ ^[0-9a-f]{64}$ ]]; then
	device_result preflight.archive-sha256 FAIL 2 "archive SHA-256 is invalid" - -
	preflight_failed=1
fi
if [[ ! -f $FIXTURE_DIR/verify-oci.sh || ! -f $FIXTURE_DIR/slow_http.py || \
	! -f $FIXTURE_DIR/Containerfile ]]; then
	device_result preflight.fixture FAIL 2 "S4 fixture sources are incomplete" - -
	preflight_failed=1
fi
if [[ -z $S4_REAL_PREFIX || $S4_REAL_PREFIX != /* || ! -x $S4_REAL_PREFIX/bin/proot ]]; then
	device_result preflight.prefix FAIL 2 "canonical Termux PREFIX/proot is unavailable" - -
	preflight_failed=1
fi
if ((preflight_failed)); then exit 1; fi

S4_REAL_PREFIX=$(realpath -e -- "$S4_REAL_PREFIX") || {
	device_result preflight.app-private FAIL 1 "cannot canonicalize the Termux PREFIX" - -
	exit 1
}
S4_APP_PRIVATE_ROOT=$(realpath -e -- "$S4_REAL_PREFIX/..") || {
	device_result preflight.app-private FAIL 1 "cannot resolve the Termux app-private root" - -
	exit 1
}
case $DEVICE_RUN_DIR in
	"$S4_APP_PRIVATE_ROOT"/*) ;;
	*)
		device_result preflight.app-private FAIL 1 \
			"S4 output root must be under the canonical Termux app-private root" - -
		exit 1
		;;
esac
mkdir -m 0700 -- "$DEVICE_RUN_DIR/sandboxes" || {
	device_result preflight.app-private FAIL 1 "cannot create the private sandbox root" - -
	exit 1
}
device_result preflight.app-private PASS 0 \
	"evidence and synthetic prefixes are under the canonical Termux app-private root" - -

archive_source=$archive
private_archive=$DEVICE_WORK_DIR/s4-fixture.oci.tar
if ! cp -- "$archive_source" "$private_archive" || ! chmod 0600 "$private_archive"; then
	device_result preflight.archive-copy FAIL 1 "cannot snapshot archive into private run storage" - -
	exit 1
fi
archive=$private_archive
device_capture_timed 90 preflight.oci bash "$FIXTURE_DIR/verify-oci.sh" \
	"$archive" "$archive_sha256"
if ((DEVICE_CAPTURE_RC == 0)); then
	device_result preflight.oci PASS 0 "checksum-pinned OCI fixture, platform and slow layer verified" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result preflight.oci FAIL "$DEVICE_CAPTURE_RC" "OCI validation failed" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	exit 1
fi

if [[ $(uname -m) != aarch64 || $(dpkg --print-architecture 2>/dev/null) != aarch64 ]]; then
	device_result preflight.architecture FAIL 1 "S4 requires native aarch64 Termux" - -
	exit 1
fi
device_result preflight.architecture PASS 0 "native aarch64 confirmed" - -

pd_version=$(dpkg-query -W -f='${Version}' proot-distro 2>/dev/null || true)
proot_version=$(dpkg-query -W -f='${Version}' proot 2>/dev/null || true)
if [[ $pd_version != 5.6.0 || $proot_version != 5.1.107.90 ]]; then
	device_result preflight.engine FAIL 1 \
		"S4 requires proot-distro 5.6.0/proot 5.1.107.90; found $pd_version/$proot_version" - -
	exit 1
fi
device_result preflight.engine PASS 0 "engine versions pinned" - -

S4_BOOT_ID=$(< /proc/sys/kernel/random/boot_id) || {
	device_result preflight.proc FAIL 1 "boot_id is unreadable" - -
	exit 1
}
device_metadata run_id "$run_id"
device_metadata archive_source "$archive_source"
device_metadata archive_sha256 "$archive_sha256"
device_metadata harness_sha256 "$(sha256sum "$SCRIPT_DIR/s4.sh" | awk '{print $1}')"
device_metadata shared_lib_sha256 "$(sha256sum "$SCRIPT_DIR/lib.sh" | awk '{print $1}')"
device_metadata validator_sha256 "$(sha256sum "$FIXTURE_DIR/verify-oci.sh" | awk '{print $1}')"
device_metadata slow_http_sha256 "$(sha256sum "$FIXTURE_DIR/slow_http.py" | awk '{print $1}')"
device_metadata containerfile_sha256 "$(sha256sum "$FIXTURE_DIR/Containerfile" | awk '{print $1}')"
device_metadata proot_distro_version "$pd_version"
device_metadata proot_version "$proot_version"
device_metadata boot_id "$S4_BOOT_ID"
device_metadata architecture "$(uname -m)"
device_metadata android_sdk "$(getprop ro.build.version.sdk 2>/dev/null || printf unavailable)"
device_metadata fault_cycles "$fault_cycles"

# C0: complete local install proves the fixture and positive public observer.
c0_ok=1
if ! s4_prepare_attempt C0 1; then
	c0_ok=0
	s4_preserve_attempt "C0 preparation or containment proof failed"
elif ! s4_launch_install "$archive" C0-launch; then
	c0_ok=0
	s4_preserve_attempt "C0 install scope could not be qualified"
else
	c0_pid=$S4_LAST_PID
	if ! s4_wait_process "$c0_pid" C0-complete 240 || [[ $S4_LAST_WAIT_RC != 0 ]]; then
		S4_INSTALL_WAIT_RC=$S4_LAST_WAIT_RC
		c0_ok=0
		if [[ ${S4_PROC_ACTIVE[$c0_pid]:-0} == 1 ]]; then
			s4_signal_scope "$c0_pid" KILL C0-timeout && \
				s4_wait_process "$c0_pid" C0-timeout-drain 30 || true
		fi
	else
		S4_INSTALL_WAIT_RC=$S4_LAST_WAIT_RC
	fi
	if ((c0_ok)); then
		S4_BARRIER_SEEN=1
		s4_observe_alias
		if [[ $S4_CLASSIFICATION != owned ]]; then c0_ok=0; fi
	fi
	if ((c0_ok)); then
		if ! s4_cleanup_owned_attempt; then c0_ok=0; fi
	fi
	if ((c0_ok == 0)); then s4_preserve_attempt "C0 was not safely owned and removed"; fi
fi
s4_record_golden complete owned
s4_result C0.complete "$c0_ok" \
	"complete fixture install classified $S4_CLASSIFICATION after two exact public observations"
if ((c0_ok == 0)); then exit 1; fi

# F1: the loopback helper sends one chunk, fsyncs its barrier, then holds.
f1_passes=0
for ((cycle = 1; cycle <= fault_cycles; cycle += 1)); do
	f1_ok=1
	f1_server=
	f1_install=
	if ! s4_prepare_attempt F1 "$cycle"; then
		f1_ok=0
		s4_preserve_attempt "F1 preparation or containment proof failed"
	else
		if s4_launch_server "F1-$cycle-server"; then
			port=$S4_SERVER_PORT
			f1_server=$S4_LAST_PID
		else
			f1_ok=0
		fi
		if ((f1_ok)) && s4_launch_install \
			"http://127.0.0.1:$port/fixture.oci.tar" "F1-$cycle-install"; then
			f1_install=$S4_LAST_PID
		else
			f1_ok=0
		fi
		if ((f1_ok)) && s4_wait_pattern "$S4_CONTROL/server.events" $'\tbarrier\t' \
			"$f1_install" 120 && grep -F -- 'Downloading archive' \
			"$S4_RAW_DIR/$S4_ATTEMPT.install.stdout" \
			"$S4_RAW_DIR/$S4_ATTEMPT.install.stderr" >/dev/null 2>&1; then
			S4_BARRIER_SEEN=1
		else
			f1_ok=0
		fi
		if ((f1_ok)) && s4_server_is_holding "$f1_server" && \
			s4_signal_scope "$f1_install" KILL "F1-$cycle-kill" && \
			s4_wait_process "$f1_install" "F1-$cycle-drain" 30; then
			S4_INSTALL_WAIT_RC=$S4_LAST_WAIT_RC
			[[ $S4_LAST_WAIT_RC == 137 ]] || f1_ok=0
		else
			f1_ok=0
		fi
		if [[ -n $f1_server ]]; then
			if ! s4_release_server "$f1_server" "F1-$cycle-server-drain" || \
				! s4_server_released_after_disconnect; then
				f1_ok=0
			fi
		fi
		if [[ -n $f1_server ]] && ! s4_capture_server_evidence; then
			f1_ok=0
		fi
		if [[ -n $f1_install && ${S4_PROC_ACTIVE[$f1_install]:-0} == 0 ]]; then
			s4_observe_alias
		else
			S4_CLASSIFICATION=ambiguous
		fi
		if [[ $S4_CLASSIFICATION != owned ]]; then f1_ok=0; fi
		if ((f1_ok)) && ! s4_cleanup_owned_attempt; then f1_ok=0; fi
		if ((f1_ok == 0)); then s4_preserve_attempt "F1 fault was ambiguous or cleanup failed"; fi
	fi
	s4_record_golden loopback-first-chunk owned
	s4_result "F1.$cycle" "$f1_ok" \
		"download SIGKILL classified $S4_CLASSIFICATION and cleanup was $S4_ATTEMPT_CLEANUP"
	if ((f1_ok)); then f1_passes=$((f1_passes + 1)); fi
done
s4_result F1.download "$((f1_passes == fault_cycles))" \
	"$f1_passes/$fault_cycles deterministic loopback download faults passed"

# F2: stop as soon as public stderr announces application of layer 2/2.
f2_passes=0
for ((cycle = 1; cycle <= fault_cycles; cycle += 1)); do
	f2_ok=1
	f2_install=
	if ! s4_prepare_attempt F2 "$cycle"; then
		f2_ok=0
		s4_preserve_attempt "F2 preparation or containment proof failed"
	elif s4_launch_install "$archive" "F2-$cycle-install"; then
		f2_install=$S4_LAST_PID
	else
		f2_ok=0
	fi
	if ((f2_ok)) && s4_wait_pattern "$S4_RAW_DIR/$S4_ATTEMPT.install.stderr" \
		'Applying layer 2/2' "$f2_install" 180; then
		S4_BARRIER_SEEN=1
	else
		f2_ok=0
	fi
	if ((f2_ok)) && s4_stop_at_layer_marker_then_kill \
		"$f2_install" "F2-$cycle-stop-kill" && \
		s4_wait_process "$f2_install" "F2-$cycle-drain" 30; then
		S4_INSTALL_WAIT_RC=$S4_LAST_WAIT_RC
		if [[ $S4_LAST_WAIT_RC != 137 ]] || ! s4_no_later_public_phase; then
			f2_ok=0
		fi
	else
		f2_ok=0
	fi
	if [[ -n $f2_install && ${S4_PROC_ACTIVE[$f2_install]:-0} == 0 ]]; then
		s4_observe_alias
	else
		S4_CLASSIFICATION=ambiguous
	fi
	if [[ $S4_CLASSIFICATION != owned ]]; then f2_ok=0; fi
	if ((f2_ok)) && ! s4_cleanup_owned_attempt; then f2_ok=0; fi
	if ((f2_ok == 0)); then s4_preserve_attempt "F2 fault was ambiguous or cleanup failed"; fi
	s4_record_golden applying-layer-2-of-2 owned
	s4_result "F2.$cycle" "$f2_ok" \
		"post-layer-marker SIGSTOP+SIGKILL classified $S4_CLASSIFICATION and cleanup was $S4_ATTEMPT_CLEANUP"
	if ((f2_ok)); then f2_passes=$((f2_passes + 1)); fi
done
s4_result F2.layer-marker "$((f2_passes == fault_cycles))" \
	"$f2_passes/$fault_cycles post-layer-marker faults passed before any later public phase marker"

if ((f1_passes == fault_cycles && f2_passes == fault_cycles)); then
	device_result S4.matrix PASS 0 \
		"C0, F1 and F2 produced attributable owned aliases; ambiguous remains fail-closed" - -
else
	device_result S4.matrix FAIL 1 \
		"one or more fault attempts were ambiguous; preserved.tsv identifies retained sandboxes" - -
fi

s4_cleanup
if ((S4_CLEANUP_FAILURES == 0)); then
	device_result cleanup.processes PASS 0 \
		"all qualified child scopes drained; ambiguous sandboxes intentionally preserved" - -
else
	device_result cleanup.processes FAIL 1 "one or more child scopes could not be drained" - -
fi
device_finish
if ((DEVICE_FAILURE_COUNT > 0 || S4_CLEANUP_FAILURES > 0)); then exit 1; fi
exit 0
