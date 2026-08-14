#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
FIXTURE_DIR=$SCRIPT_DIR/fixtures/s3

DEVICE_PHASE=S3
DEVICE_RUN_LABEL=termux-stacks-s3
DEVICE_RUNTIME_LABEL=txs-s3
DEVICE_HARNESS_VERSION=1
DEVICE_AUTOMATIC_SCOPE=$'The harness used one private synthetic TERMUX__PREFIX and one disposable\nexact-name alias. It qualified exact session-ID stop against cooperative,\nTERM-ignoring, escaped-session, same-alias and dead-PRoot workloads. It never\nused an alias target, --all, or a host PID/PGID as a production fallback.'

# shellcheck source=tests/device/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
	cat <<'EOF'
Usage:
  bash tests/device/s3.sh --archive ABSOLUTE_OCI_ARCHIVE \
    --archive-sha256 LOWERCASE_SHA256 [--output-root ABSOLUTE_DIR] \
    [--stress-cycles 0..100]

S3 mutates only a private synthetic proot-distro runtime. The sole candidate
production stop is `proot-distro kill SESSION_PID`; alias and --all targets are
never used. A direct PGID TERM is executed once as a negative control only.
EOF
}

archive=
archive_sha256=
output_root=
stress_cycles=100
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
		--stress-cycles)
			[[ $# -ge 2 ]] || { device_error "--stress-cycles requires a value"; exit 2; }
			stress_cycles=$2
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
if [[ ! $stress_cycles =~ ^(0|[1-9][0-9]{0,2})$ ]]; then
	device_error "--stress-cycles must be an integer from 0 through 100"
	exit 2
fi
stress_cycles=$((10#$stress_cycles))
if ((stress_cycles > 100)); then
	device_error "--stress-cycles must be an integer from 0 through 100"
	exit 2
fi

device_init "$output_root" || exit $?

S3_RAW_DIR=$DEVICE_EVIDENCE_DIR/signal
S3_ORACLE_FILE=$DEVICE_EVIDENCE_DIR/oracle.tsv
S3_INTENT_FILE=$DEVICE_EVIDENCE_DIR/intent.tsv
S3_CLEANUP_FILE=$DEVICE_EVIDENCE_DIR/cleanup.raw
S3_REAL_PRE=$DEVICE_EVIDENCE_DIR/real-containers.pre
S3_REAL_POST=$DEVICE_EVIDENCE_DIR/real-containers.post
mkdir -m 0700 -- "$S3_RAW_DIR"
printf 'phase\ttoken\trole\tpid\tstarttime\tppid\tpgid\tsid\tstate\tcomm\n' >"$S3_ORACLE_FILE"
printf 'time_utc\taction\ttarget\n' >"$S3_INTENT_FILE"
: >"$S3_CLEANUP_FILE"

run_id=$(printf '%x%04x' "$(date +%s)" "$RANDOM")
S3_ALIAS=txs-s3-$run_id-worker
S3_SANDBOX=$DEVICE_RUN_DIR/sandbox
S3_PREFIX=$S3_SANDBOX/prefix
S3_HOME=$S3_SANDBOX/home
S3_CONTROL=$S3_SANDBOX/control
S3_SENTINEL=$S3_SANDBOX/.termux-stacks-s3-sentinel
S3_SENTINEL_VALUE=$run_id-$RANDOM-$RANDOM
S3_SESSIONS=$S3_PREFIX/var/lib/proot-distro/sessions
S3_REAL_PREFIX=${PREFIX:-}
S3_BOOT_ID=
S3_SANDBOX_ID=
S3_SESSIONS_ID=
S3_CONTAINMENT_PROVEN=0
S3_ALIAS_OWNED=0
S3_CLEANUP_STATE=disabled
S3_CLEANUP_FAILURES=0
S3_PRESERVE_SANDBOX=0
S3_DEFERRED_SIGNAL=0
S3_LAST_PID=
S3_LAST_KILL_MS=0

declare -a S3_PIDS=()
declare -A S3_TOKEN=()
declare -A S3_MODE=()
declare -A S3_ACTIVE=()
declare -A S3_QUALIFIED=()
declare -A S3_ROOT_STARTTIME=()
declare -A S3_ROOT_PGID=()
declare -A S3_ROOT_SID=()
declare -A S3_RECORD_ID=()
declare -A S3_ROLE_PID=()
declare -A S3_ROLE_STARTTIME=()
declare -A S3_ROLE_PPID=()
declare -A S3_ROLE_PGID=()
declare -A S3_ROLE_SID=()

s3_defer_signal() {
	local code=$1
	if ((S3_DEFERRED_SIGNAL == 0)); then S3_DEFERRED_SIGNAL=$code; fi
}

s3_install_deferred_signal_handlers() {
	trap 's3_defer_signal 129' HUP
	trap 's3_defer_signal 130' INT
	trap 's3_defer_signal 143' TERM
}

s3_intent() {
	printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$2" \
		>>"$S3_INTENT_FILE"
}

s3_pd() {
	timeout --signal=KILL 15 env \
		TERMUX__PREFIX="$S3_PREFIX" \
		TERMUX__HOME="$S3_HOME" \
		PD_PROOT_BIN="$S3_REAL_PREFIX/bin/proot" \
		PD_FORCE_NO_COLORS=true \
		COLUMNS=240 \
		proot-distro "$@"
}

s3_proc_fields() {
	local pid=$1
	local line rest
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

s3_identity_matches_values() {
	local pid=$1 expected_start=$2 expected_pgid=$3 expected_sid=$4
	local fields starttime _ppid pgid sid state boot_now
	boot_now=$(< /proc/sys/kernel/random/boot_id) || return 2
	[[ $boot_now == "$S3_BOOT_ID" ]] || return 2
	fields=$(s3_proc_fields "$pid") || return 1
	IFS=$'\t' read -r starttime _ppid pgid sid state <<<"$fields"
	[[ $starttime == "$expected_start" && $pgid == "$expected_pgid" && \
		$sid == "$expected_sid" && $state != Z && $state != X && $state != x ]]
}

s3_root_identity_matches() {
	local pid=$1
	[[ ${S3_ROOT_STARTTIME[$pid]+set} ]] || return 2
	s3_identity_matches_values "$pid" "${S3_ROOT_STARTTIME[$pid]}" \
		"${S3_ROOT_PGID[$pid]}" "${S3_ROOT_SID[$pid]}"
}

s3_sessions_identity_matches() {
	local canonical id_now
	[[ -n $S3_SESSIONS_ID && -d $S3_SESSIONS && ! -L $S3_SESSIONS ]] || return 1
	canonical=$(realpath -e -- "$S3_SESSIONS" 2>/dev/null) || return 1
	id_now=$(stat -c '%d:%i' -- "$S3_SESSIONS" 2>/dev/null) || return 1
	[[ $canonical == "$S3_SESSIONS" && $id_now == "$S3_SESSIONS_ID" ]]
}

s3_record_identity_matches() {
	local pid=$1
	local record=$S3_SESSIONS/$pid.json
	local canonical id_now record_pid record_alias
	[[ ${S3_RECORD_ID[$pid]+set} && -f $record && ! -L $record ]] || return 1
	canonical=$(realpath -e -- "$record" 2>/dev/null) || return 1
	[[ $canonical == "$record" ]] || return 1
	id_now=$(stat -c '%d:%i' -- "$record" 2>/dev/null) || return 1
	[[ $id_now == "${S3_RECORD_ID[$pid]}" ]] || return 1
	record_pid=$(jq -er '.pid' "$record" 2>/dev/null) || return 1
	record_alias=$(jq -er '.container' "$record" 2>/dev/null) || return 1
	[[ $record_pid == "$pid" && $record_alias == "$S3_ALIAS" ]]
}

s3_holders() {
	local record_id=$1
	python -c '
import os, sys
try:
    wanted_dev, wanted_ino = (int(v) for v in sys.argv[1].split(":"))
except Exception:
    raise SystemExit(2)
holders = []
try:
    proc_entries = os.scandir("/proc")
except OSError:
    raise SystemExit(2)
with proc_entries:
    for proc in proc_entries:
        if not proc.name.isdigit():
            continue
        try:
            with os.scandir(f"/proc/{proc.name}/fd") as fds:
                for fd in fds:
                    try:
                        st = os.stat(fd.path)
                    except OSError:
                        continue
                    if st.st_dev == wanted_dev and st.st_ino == wanted_ino:
                        holders.append(int(proc.name))
                        break
        except OSError:
            continue
for pid in sorted(set(holders)):
    print(pid)
' "$record_id"
}

s3_holders_capture() {
	local pid=$1 phase=$2
	local target=$S3_RAW_DIR/$phase.holders
	[[ ${S3_RECORD_ID[$pid]+set} ]] || return 2
	if ! s3_holders "${S3_RECORD_ID[$pid]}" >"$target" 2>"$target.stderr"; then
		return 2
	fi
}

s3_ps_contains() {
	grep -Fx -- "$2" "$1" >/dev/null 2>&1
}

s3_capture_ps() {
	local phase=$1
	device_capture_timed 15 "$phase" env \
		TERMUX__PREFIX="$S3_PREFIX" TERMUX__HOME="$S3_HOME" \
		PD_PROOT_BIN="$S3_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
		COLUMNS=240 proot-distro ps --quiet
}

s3_role_key() {
	printf '%s:%s' "$1" "$2"
}

s3_read_role() {
	local session_pid=$1 role=$2 token=${S3_TOKEN[$1]} mode=${S3_MODE[$1]}
	local file=$S3_CONTROL/$token.$role.identity
	local schema file_token file_role pid starttime ppid pgid sid state file_mode _now
	local fields actual_start actual_ppid actual_pgid actual_sid actual_state comm key
	[[ -f $file && ! -L $file ]] || return 1
	IFS=$'\t' read -r schema file_token file_role pid starttime ppid pgid sid state \
		file_mode _now <"$file" || return 1
	[[ $schema == v1 && $file_token == "$token" && $file_role == "$role" && \
		$file_mode == "$mode" && $pid =~ ^[1-9][0-9]*$ && \
		$starttime =~ ^[0-9]+$ && $ppid =~ ^[0-9]+$ && \
		$pgid =~ ^[1-9][0-9]*$ && $sid =~ ^[1-9][0-9]*$ ]] || return 1
	fields=$(s3_proc_fields "$pid") || return 1
	IFS=$'\t' read -r actual_start actual_ppid actual_pgid actual_sid actual_state <<<"$fields"
	[[ $actual_start == "$starttime" && $actual_ppid == "$ppid" && \
		$actual_pgid == "$pgid" && $actual_sid == "$sid" && \
		$actual_state != Z && $actual_state != X && $actual_state != x ]] || return 1
	IFS= read -r comm <"/proc/$pid/comm" || return 1
	key=$(s3_role_key "$session_pid" "$role")
	S3_ROLE_PID[$key]=$pid
	S3_ROLE_STARTTIME[$key]=$starttime
	S3_ROLE_PPID[$key]=$ppid
	S3_ROLE_PGID[$key]=$pgid
	S3_ROLE_SID[$key]=$sid
	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
		qualified "$token" "$role" "$pid" "$starttime" "$ppid" "$pgid" "$sid" \
		"$actual_state" "$comm" >>"$S3_ORACLE_FILE"
}

s3_role_alive() {
	local session_pid=$1 role=$2 key
	key=$(s3_role_key "$session_pid" "$role")
	[[ ${S3_ROLE_PID[$key]+set} ]] || return 2
	s3_identity_matches_values "${S3_ROLE_PID[$key]}" "${S3_ROLE_STARTTIME[$key]}" \
		"${S3_ROLE_PGID[$key]}" "${S3_ROLE_SID[$key]}"
}

s3_all_roles_alive() {
	local session_pid=$1 role
	for role in root child grandchild; do
		s3_role_alive "$session_pid" "$role" || return 1
	done
}

s3_roles_gone() {
	local session_pid=$1 role rc
	for role in root child grandchild; do
		s3_role_alive "$session_pid" "$role"
		rc=$?
		case $rc in
			0) return 1 ;;
			1) ;;
			*) return 2 ;;
		esac
	done
}

s3_calibrate_holders() {
	local session_pid=$1 phase=$2 role key holder
	s3_holders_capture "$session_pid" "$phase" || return 1
	[[ -s $S3_RAW_DIR/$phase.holders ]] || return 1
	for role in root child grandchild; do
		key=$(s3_role_key "$session_pid" "$role")
		holder=${S3_ROLE_PID[$key]:-}
		[[ -n $holder ]] && grep -Fx -- "$holder" "$S3_RAW_DIR/$phase.holders" >/dev/null || \
			return 1
	done
}

s3_scope_empty() {
	local session_pid=$1 phase=$2
	local root_pgid=${S3_ROOT_PGID[$session_pid]:-}
	local root_sid=${S3_ROOT_SID[$session_pid]:-}
	local grandchild_key grandchild_pgid grandchild_sid scan_rc
	local snapshot=$S3_RAW_DIR/$phase.scope
	[[ $root_pgid =~ ^[1-9][0-9]*$ && $root_sid =~ ^[1-9][0-9]*$ ]] || return 2
	grandchild_key=$(s3_role_key "$session_pid" grandchild)
	grandchild_pgid=${S3_ROLE_PGID[$grandchild_key]:-$root_pgid}
	grandchild_sid=${S3_ROLE_SID[$grandchild_key]:-$root_sid}
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 2
	awk -v root_group="$root_pgid" -v root_session="$root_sid" \
		-v extra_group="$grandchild_pgid" -v extra_session="$grandchild_sid" '
		$2 == root_group || $3 == root_session ||
		$2 == extra_group || $3 == extra_session { found = 1 }
		END { exit found ? 0 : 1 }
	' "$snapshot"
	scan_rc=$?
	case $scan_rc in
		0) return 1 ;;
		1) return 0 ;;
		*) return 2 ;;
	esac
}

s3_calibrate_scope() {
	local session_pid=$1 phase=$2
	local role key pid pgid sid snapshot=$S3_RAW_DIR/$phase.scope-calibration
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 1
	awk -v wanted_pid="$session_pid" -v wanted_pgid="${S3_ROOT_PGID[$session_pid]}" \
		-v wanted_sid="${S3_ROOT_SID[$session_pid]}" '
		$1 == wanted_pid && $2 == wanted_pgid && $3 == wanted_sid { found = 1 }
		END { exit found ? 0 : 1 }
	' "$snapshot" || return 1
	for role in root child grandchild; do
		key=$(s3_role_key "$session_pid" "$role")
		pid=${S3_ROLE_PID[$key]}
		pgid=${S3_ROLE_PGID[$key]}
		sid=${S3_ROLE_SID[$key]}
		awk -v wanted_pid="$pid" -v wanted_pgid="$pgid" -v wanted_sid="$sid" '
			$1 == wanted_pid && $2 == wanted_pgid && $3 == wanted_sid { found = 1 }
			END { exit found ? 0 : 1 }
		' "$snapshot" || return 1
	done
}

s3_capture_control() {
	local session_pid=$1 phase=$2
	local token=${S3_TOKEN[$session_pid]:-}
	local destination=$S3_RAW_DIR/control/$token suffix source
	[[ $token =~ ^[a-z0-9][a-z0-9-]{0,47}$ ]] || return 1
	mkdir -m 0700 -p -- "$destination" || return 1
	for suffix in events ready root.identity child.identity grandchild.identity stop; do
		source=$S3_CONTROL/$token.$suffix
		if [[ -e $source ]]; then
			[[ -f $source && ! -L $source ]] || return 1
			cp -- "$source" "$destination/$suffix" || return 1
		fi
	done
	printf '%s\n' "$phase" >"$destination/captured-at-phase" || return 1
}

s3_launch() {
	local phase=$1 token=$2 mode=$3
	local pid fields starttime _ppid pgid sid state comm record record_id
	local root_key child_key grandchild_key iteration
	[[ $token =~ ^[a-z0-9][a-z0-9-]{0,47}$ ]] || return 1
	case $mode in cooperate | ignore | escape | orphan) ;; *) return 1 ;; esac
	for role in root child grandchild; do
		[[ ! -e $S3_CONTROL/$token.$role.identity ]] || return 1
	done
	[[ ! -e $S3_CONTROL/$token.ready && ! -e $S3_CONTROL/$token.events && \
		! -e $S3_CONTROL/$token.stop ]] || return 1

	s3_intent launch-session "$phase:$token:$mode"
	setsid env \
		TERMUX__PREFIX="$S3_PREFIX" \
		TERMUX__HOME="$S3_HOME" \
		PD_PROOT_BIN="$S3_REAL_PREFIX/bin/proot" \
		PD_FORCE_NO_COLORS=true \
		COLUMNS=240 \
		proot-distro run --isolated \
		--bind "$S3_CONTROL:/control" \
		--env "TSTACK_S3_TOKEN=$token" \
		--env "TSTACK_S3_MODE=$mode" \
		--env TSTACK_S3_TTL=120 \
		"$S3_ALIAS" \
		>"$S3_RAW_DIR/$phase.run.stdout" \
		2>"$S3_RAW_DIR/$phase.run.stderr" &
	pid=$!
	S3_PIDS+=("$pid")
	S3_TOKEN[$pid]=$token
	S3_MODE[$pid]=$mode
	S3_ACTIVE[$pid]=1
	S3_QUALIFIED[$pid]=0
	S3_LAST_PID=$pid

	for ((iteration = 0; iteration < 100; iteration += 1)); do
		if [[ -f $S3_CONTROL/$token.ready && \
			-f $S3_CONTROL/$token.root.identity && \
			-f $S3_CONTROL/$token.child.identity && \
			-f $S3_CONTROL/$token.grandchild.identity ]]; then
			break
		fi
		kill -0 "$pid" 2>/dev/null || break
		sleep 0.1
	done
	[[ -f $S3_CONTROL/$token.ready ]] || return 1

	fields=$(s3_proc_fields "$pid") || return 1
	IFS=$'\t' read -r starttime _ppid pgid sid state <<<"$fields"
	[[ $pgid == "$pid" && $sid == "$pid" && $state != Z && $state != X ]] || return 1
	IFS= read -r comm <"/proc/$pid/comm" || return 1
	[[ $comm == proot ]] || return 1
	S3_ROOT_STARTTIME[$pid]=$starttime
	S3_ROOT_PGID[$pid]=$pgid
	S3_ROOT_SID[$pid]=$sid
	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
		qualified "$token" tracer "$pid" "$starttime" "$_ppid" "$pgid" "$sid" \
		"$state" "$comm" >>"$S3_ORACLE_FILE"

	s3_read_role "$pid" root || return 1
	s3_read_role "$pid" child || return 1
	s3_read_role "$pid" grandchild || return 1
	root_key=$(s3_role_key "$pid" root)
	child_key=$(s3_role_key "$pid" child)
	grandchild_key=$(s3_role_key "$pid" grandchild)
	[[ ${S3_ROLE_PID[$root_key]} != "${S3_ROLE_PID[$child_key]}" && \
		${S3_ROLE_PID[$root_key]} != "${S3_ROLE_PID[$grandchild_key]}" && \
		${S3_ROLE_PID[$child_key]} != "${S3_ROLE_PID[$grandchild_key]}" ]] || return 1
	[[ ${S3_ROLE_PPID[$root_key]} == "$pid" && \
		${S3_ROLE_PPID[$child_key]} == "${S3_ROLE_PID[$root_key]}" && \
		${S3_ROLE_PPID[$grandchild_key]} == "${S3_ROLE_PID[$child_key]}" ]] || return 1
	[[ ${S3_ROLE_PGID[$root_key]} == "$pid" && ${S3_ROLE_SID[$root_key]} == "$pid" && \
		${S3_ROLE_PGID[$child_key]} == "$pid" && ${S3_ROLE_SID[$child_key]} == "$pid" ]] || return 1
	if [[ $mode == escape ]]; then
		[[ ${S3_ROLE_PGID[$grandchild_key]} == "${S3_ROLE_PID[$grandchild_key]}" && \
			${S3_ROLE_SID[$grandchild_key]} == "${S3_ROLE_PID[$grandchild_key]}" ]] || return 1
	else
		[[ ${S3_ROLE_PGID[$grandchild_key]} == "$pid" && \
			${S3_ROLE_SID[$grandchild_key]} == "$pid" ]] || return 1
	fi
	s3_calibrate_scope "$pid" "$phase" || return 1

	record=$S3_SESSIONS/$pid.json
	[[ -f $record && ! -L $record ]] || return 1
	record_id=$(stat -c '%d:%i' -- "$record") || return 1
	S3_RECORD_ID[$pid]=$record_id
	s3_record_identity_matches "$pid" || return 1
	if flock -n "$record" true 2>/dev/null; then return 1; fi
	s3_calibrate_holders "$pid" "$phase.calibration" || return 1
	s3_capture_ps "$phase.ps-live"
	[[ $DEVICE_CAPTURE_RC == 0 ]] && s3_ps_contains "$DEVICE_CAPTURE_STDOUT" "$pid" || return 1
	S3_QUALIFIED[$pid]=1
}

s3_authorize_engine_kill() {
	local pid=$1 phase=$2
	[[ ${S3_ACTIVE[$pid]:-0} == 1 && ${S3_QUALIFIED[$pid]:-0} == 1 ]] || return 1
	s3_sessions_identity_matches || return 1
	s3_record_identity_matches "$pid" || return 1
	s3_holders_capture "$pid" "$phase.authorize" || return 1
	[[ -s $S3_RAW_DIR/$phase.authorize.holders ]] || return 1
	s3_capture_ps "$phase.authorize-ps"
	[[ $DEVICE_CAPTURE_RC == 0 ]] && s3_ps_contains "$DEVICE_CAPTURE_STDOUT" "$pid"
}

s3_reap_child_if_dead() {
	local pid=$1 fields _start _ppid _pgid _sid state
	if [[ ! -e /proc/$pid/stat ]]; then
		wait "$pid" 2>/dev/null || true
		return 0
	fi
	fields=$(s3_proc_fields "$pid") || return 2
	IFS=$'\t' read -r _start _ppid _pgid _sid state <<<"$fields"
	if [[ $state == Z || $state == X || $state == x ]]; then
		wait "$pid" 2>/dev/null || true
		return 0
	fi
	return 1
}

s3_wait_drained() {
	local pid=$1 phase=$2 iteration holders_rc roles_rc scope_rc
	for ((iteration = 0; iteration < 120; iteration += 1)); do
		s3_roles_gone "$pid"
		roles_rc=$?
		((roles_rc == 2)) && return 1
		s3_holders_capture "$pid" "$phase.poll-$iteration"
		holders_rc=$?
		((holders_rc == 0)) || return 1
		s3_scope_empty "$pid" "$phase.poll-$iteration"
		scope_rc=$?
		((scope_rc == 2)) && return 1
		if ((roles_rc == 0 && scope_rc == 0)) && \
			[[ ! -s $S3_RAW_DIR/$phase.poll-$iteration.holders ]]; then
			s3_reap_child_if_dead "$pid" || return 1
			return 0
		fi
		sleep 0.1
	done
	return 1
}

s3_engine_kill() {
	local phase=$1 pid=$2 signal=${3:-TERM}
	local before after
	s3_authorize_engine_kill "$pid" "$phase" || return 1
	s3_intent engine-kill "$signal:$pid"
	before=$(date +%s%3N) || return 1
	if [[ $signal == TERM ]]; then
		device_capture_timed 15 "$phase.kill" env \
			TERMUX__PREFIX="$S3_PREFIX" TERMUX__HOME="$S3_HOME" \
			PD_PROOT_BIN="$S3_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
			proot-distro kill "$pid"
	else
		device_capture_timed 15 "$phase.kill" env \
			TERMUX__PREFIX="$S3_PREFIX" TERMUX__HOME="$S3_HOME" \
			PD_PROOT_BIN="$S3_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
			proot-distro kill --signal "$signal" "$pid"
	fi
	after=$(date +%s%3N) || return 1
	S3_LAST_KILL_MS=$((after - before))
	[[ $DEVICE_CAPTURE_RC == 0 ]] || return 1
	s3_wait_drained "$pid" "$phase.drain" || return 1
	s3_capture_control "$pid" "$phase" || return 1
	s3_capture_ps "$phase.prune"
	[[ $DEVICE_CAPTURE_RC == 0 ]] || return 1
	s3_ps_contains "$DEVICE_CAPTURE_STDOUT" "$pid" && return 1
	[[ ! -e $S3_SESSIONS/$pid.json ]] || return 1
	S3_ACTIVE[$pid]=0
}

s3_result() {
	local id=$1 ok=$2 detail=$3
	if ((ok)); then
		device_result "$id" PASS 0 "$detail" - -
	else
		device_result "$id" FAIL 1 "$detail" - -
	fi
}

s3_authorize_group_term() {
	local pid=$1 phase=$2
	local root_key child_key root_worker child_worker snapshot
	root_key=$(s3_role_key "$pid" root)
	child_key=$(s3_role_key "$pid" child)
	root_worker=${S3_ROLE_PID[$root_key]:-}
	child_worker=${S3_ROLE_PID[$child_key]:-}
	[[ $pid != "$$" && ${S3_ROOT_PGID[$pid]:-} == "$pid" && \
		${S3_ROOT_SID[$pid]:-} == "$pid" ]] || return 1
	s3_root_identity_matches "$pid" || return 1
	s3_role_alive "$pid" root || return 1
	s3_role_alive "$pid" child || return 1
	snapshot=$S3_RAW_DIR/$phase.group-authorize
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 1
	awk -v group="$pid" -v session="$pid" -v tracer="$pid" \
		-v root_worker="$root_worker" -v child_worker="$child_worker" '
		$2 == group { count += 1; if ($3 != session) bad = 1 }
		$1 == tracer && $2 == group && $3 == session { have_tracer = 1 }
		$1 == root_worker && $2 == group && $3 == session { have_root = 1 }
		$1 == child_worker && $2 == group && $3 == session { have_child = 1 }
		END {
			exit (count > 0 && !bad && have_tracer && have_root && have_child) ? 0 : 1
		}
	' "$snapshot"
}

s3_cleanup_session() {
	local pid=$1 token=${S3_TOKEN[$1]:-} iteration
	[[ ${S3_ACTIVE[$pid]:-0} == 1 ]] || return 0
	if [[ -n $token && $token =~ ^[a-z0-9][a-z0-9-]{0,47}$ ]]; then
		: >"$S3_CONTROL/$token.stop" 2>/dev/null || true
	fi
	if [[ ${S3_QUALIFIED[$pid]:-0} == 1 ]]; then
		if s3_wait_drained "$pid" "cleanup-$pid-control"; then
			s3_capture_control "$pid" cleanup-control || return 1
			s3_capture_ps "cleanup-$pid-prune"
			if [[ $DEVICE_CAPTURE_RC == 0 ]] && ! s3_ps_contains "$DEVICE_CAPTURE_STDOUT" "$pid"; then
				S3_ACTIVE[$pid]=0
				return 0
			fi
		fi
		if s3_engine_kill "cleanup-$pid-engine" "$pid" TERM; then return 0; fi
	fi
	printf 'AMBIGUOUS\t%s\twaiting for fixture TTL; no broader signal authorized\n' "$pid" \
		>>"$S3_CLEANUP_FILE"
	for ((iteration = 0; iteration < 1250; iteration += 1)); do
		sleep 0.1
	done
	s3_capture_control "$pid" cleanup-ambiguous 2>/dev/null || true
	return 1
}

s3_cleanup() {
	local pid canonical id_now current
	[[ $S3_CLEANUP_STATE == pending ]] || return 0
	s3_install_deferred_signal_handlers
	S3_CLEANUP_STATE=running
	printf 'cleanup_started\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$S3_CLEANUP_FILE"
	for pid in "${S3_PIDS[@]}"; do
		if ! s3_cleanup_session "$pid"; then
			printf 'LIVE_OR_AMBIGUOUS\t%s\n' "$pid" >>"$S3_CLEANUP_FILE"
			S3_CLEANUP_FAILURES=$((S3_CLEANUP_FAILURES + 1))
			S3_PRESERVE_SANDBOX=1
		fi
	done
	if ((S3_CLEANUP_FAILURES == 0 && S3_PRESERVE_SANDBOX == 0 && \
		S3_CONTAINMENT_PROVEN == 1 && S3_ALIAS_OWNED == 1)); then
		if s3_pd list --quiet >"$DEVICE_RUNTIME_DIR/s3-containers.current" \
			2>>"$S3_CLEANUP_FILE"; then
			if grep -Fx -- "$S3_ALIAS" "$DEVICE_RUNTIME_DIR/s3-containers.current" >/dev/null; then
				s3_intent remove-container "$S3_ALIAS"
				s3_pd remove --quiet "$S3_ALIAS" >>"$S3_CLEANUP_FILE" 2>&1 || \
					S3_CLEANUP_FAILURES=$((S3_CLEANUP_FAILURES + 1))
			fi
		else
			S3_CLEANUP_FAILURES=$((S3_CLEANUP_FAILURES + 1))
		fi
	fi
	if ((S3_CLEANUP_FAILURES == 0 && S3_PRESERVE_SANDBOX == 0)); then
		canonical=$(realpath -e -- "$S3_SANDBOX" 2>/dev/null || true)
		id_now=$(stat -c '%d:%i' -- "$S3_SANDBOX" 2>/dev/null || true)
		if [[ $canonical == "$S3_SANDBOX" && ! -L $S3_SANDBOX && \
			$id_now == "$S3_SANDBOX_ID" && -f $S3_SENTINEL ]] && \
			[[ $(<"$S3_SENTINEL") == "$S3_SENTINEL_VALUE" ]]; then
			if ! rm -rf -- "$S3_SANDBOX" || [[ -e $S3_SANDBOX || -L $S3_SANDBOX ]]; then
				printf 'AMBIGUOUS\tsandbox removal failed or remained partial\n' \
					>>"$S3_CLEANUP_FILE"
				S3_CLEANUP_FAILURES=$((S3_CLEANUP_FAILURES + 1))
				S3_PRESERVE_SANDBOX=1
			fi
		else
			printf 'AMBIGUOUS\tsandbox identity changed; not removed\n' >>"$S3_CLEANUP_FILE"
			S3_CLEANUP_FAILURES=$((S3_CLEANUP_FAILURES + 1))
			S3_PRESERVE_SANDBOX=1
		fi
	else
		S3_PRESERVE_SANDBOX=1
	fi
	env PD_FORCE_NO_COLORS=true proot-distro list --quiet >"$S3_REAL_POST" 2>&1 || \
		S3_CLEANUP_FAILURES=$((S3_CLEANUP_FAILURES + 1))
	if grep -Fx -- "$S3_ALIAS" "$S3_REAL_POST" >/dev/null 2>&1; then
		printf 'REAL_RUNTIME_COLLISION\t%s\n' "$S3_ALIAS" >>"$S3_CLEANUP_FILE"
		S3_CLEANUP_FAILURES=$((S3_CLEANUP_FAILURES + 1))
	fi
	printf 'cleanup_finished\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$S3_CLEANUP_FILE"
	S3_CLEANUP_STATE=done
}

s3_on_exit() {
	local original_rc=$1 was_pending=0
	s3_install_deferred_signal_handlers
	trap - EXIT
	[[ $S3_CLEANUP_STATE == pending ]] && was_pending=1
	s3_cleanup
	if ((was_pending)) && ((DEVICE_FINISHED == 0)); then
		if ((S3_CLEANUP_FAILURES == 0)); then
			device_result cleanup.objects PASS 0 "owned sessions, alias and sandbox removed" - -
		else
			device_result cleanup.objects FAIL 1 "cleanup incomplete; sandbox preserved" - -
		fi
	fi
	if ((DEVICE_FINISHED == 0)); then device_finish || true; fi
	device_cleanup
	if ((S3_CLEANUP_FAILURES > 0 || DEVICE_FAILURE_COUNT > 0)) && ((original_rc == 0)); then
		original_rc=1
	fi
	if ((S3_DEFERRED_SIGNAL > 0)); then original_rc=$S3_DEFERRED_SIGNAL; fi
	trap - HUP INT TERM
	exit "$original_rc"
}

trap 's3_on_exit $?' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

preflight_failed=0
for command_name in proot-distro proot timeout sha256sum tar gzip jq setsid ps \
	stat realpath flock dpkg-query awk grep find cp python date; do
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
if [[ ! -x $FIXTURE_DIR/verify-oci.sh || ! -f $FIXTURE_DIR/worker ]]; then
	device_result preflight.fixture FAIL 2 "S3 fixture or validator is missing" - -
	preflight_failed=1
fi
if [[ -z $S3_REAL_PREFIX || $S3_REAL_PREFIX != /* || ! -x $S3_REAL_PREFIX/bin/proot ]]; then
	device_result preflight.prefix FAIL 2 "canonical Termux PREFIX/proot is unavailable" - -
	preflight_failed=1
fi
if ((preflight_failed)); then device_finish; exit 1; fi

archive_source=$archive
private_archive=$DEVICE_WORK_DIR/s3-fixture.oci.tar
if ! cp -- "$archive_source" "$private_archive" || ! chmod 0600 "$private_archive"; then
	device_result preflight.archive-copy FAIL 1 "cannot snapshot archive into private run storage" - -
	device_finish
	exit 1
fi
archive=$private_archive
device_capture_timed 30 preflight.oci bash "$FIXTURE_DIR/verify-oci.sh" \
	"$archive" "$archive_sha256"
if ((DEVICE_CAPTURE_RC == 0)); then
	device_result preflight.oci PASS 0 "blessed OCI archive, platform, blobs and worker verified" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result preflight.oci FAIL "$DEVICE_CAPTURE_RC" "OCI validation failed" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	device_finish
	exit 1
fi

if [[ $(uname -m) != aarch64 || $(dpkg --print-architecture 2>/dev/null) != aarch64 ]]; then
	device_result preflight.architecture FAIL 1 "S3 requires native aarch64 Termux" - -
	device_finish
	exit 1
fi
device_result preflight.architecture PASS 0 "native aarch64 confirmed" - -

pd_version=$(dpkg-query -W -f='${Version}' proot-distro 2>/dev/null || true)
proot_version=$(dpkg-query -W -f='${Version}' proot 2>/dev/null || true)
if [[ $pd_version != 5.6.0 || $proot_version != 5.1.107.90 ]]; then
	device_result preflight.engine FAIL 1 \
		"S3 requires proot-distro 5.6.0/proot 5.1.107.90; found $pd_version/$proot_version" - -
	device_finish
	exit 1
fi
device_result preflight.engine PASS 0 "engine versions pinned" - -

S3_BOOT_ID=$(< /proc/sys/kernel/random/boot_id) || {
	device_result preflight.proc FAIL 1 "boot_id is unreadable" - -
	device_finish
	exit 1
}
device_metadata run_id "$run_id"
device_metadata archive_source "$archive_source"
device_metadata archive_sha256 "$archive_sha256"
device_metadata harness_sha256 "$(sha256sum "$SCRIPT_DIR/s3.sh" | awk '{print $1}')"
device_metadata shared_lib_sha256 "$(sha256sum "$SCRIPT_DIR/lib.sh" | awk '{print $1}')"
device_metadata validator_sha256 "$(sha256sum "$FIXTURE_DIR/verify-oci.sh" | awk '{print $1}')"
device_metadata worker_sha256 "$(sha256sum "$FIXTURE_DIR/worker" | awk '{print $1}')"
device_metadata containerfile_sha256 "$(sha256sum "$FIXTURE_DIR/Containerfile" | awk '{print $1}')"
device_metadata proot_distro_version "$pd_version"
device_metadata proot_version "$proot_version"
device_metadata boot_id "$S3_BOOT_ID"
device_metadata architecture "$(uname -m)"
device_metadata android_sdk "$(getprop ro.build.version.sdk 2>/dev/null || printf unavailable)"
device_metadata stress_cycles "$stress_cycles"

mkdir -m 0700 -- "$S3_SANDBOX" "$S3_PREFIX" "$S3_HOME" "$S3_CONTROL"
printf '%s\n' "$S3_SENTINEL_VALUE" >"$S3_SENTINEL"
S3_SANDBOX=$(realpath -e -- "$S3_SANDBOX") || exit 1
S3_PREFIX=$S3_SANDBOX/prefix
S3_HOME=$S3_SANDBOX/home
S3_CONTROL=$S3_SANDBOX/control
S3_SENTINEL=$S3_SANDBOX/.termux-stacks-s3-sentinel
S3_SESSIONS=$S3_PREFIX/var/lib/proot-distro/sessions
S3_SANDBOX_ID=$(stat -c '%d:%i' -- "$S3_SANDBOX") || exit 1
S3_CLEANUP_STATE=pending

env PD_FORCE_NO_COLORS=true proot-distro list --quiet >"$S3_REAL_PRE" 2>&1 || {
	device_result preflight.real-runtime FAIL 1 "real container inventory failed" - -
	exit 1
}
if grep -Fx -- "$S3_ALIAS" "$S3_REAL_PRE" >/dev/null; then
	device_result preflight.real-runtime FAIL 1 "random alias exists in real runtime" - -
	exit 1
fi
device_result preflight.real-runtime PASS 0 "exact alias absent from real runtime" - -

device_capture_timed 15 preflight.synthetic-help env \
	TERMUX__PREFIX="$S3_PREFIX" TERMUX__HOME="$S3_HOME" \
	PD_PROOT_BIN="$S3_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
	proot-distro help
if ((DEVICE_CAPTURE_RC == 0)) && grep -F -- "$S3_PREFIX/var/lib/proot-distro" \
	"$DEVICE_CAPTURE_STDOUT" "$DEVICE_CAPTURE_STDERR" >/dev/null; then
	device_result preflight.synthetic-help PASS 0 "engine data location contained in sandbox" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	S3_CONTAINMENT_PROVEN=1
else
	device_result preflight.synthetic-help FAIL "$DEVICE_CAPTURE_RC" \
		"cannot prove synthetic engine data location" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	exit 1
fi

if s3_pd list --quiet >"$S3_RAW_DIR/synthetic-containers.pre" \
	2>"$S3_RAW_DIR/synthetic-containers.pre.stderr" && \
	[[ ! -s $S3_RAW_DIR/synthetic-containers.pre ]]; then
	device_result preflight.synthetic-empty PASS 0 "synthetic runtime starts empty" - -
else
	device_result preflight.synthetic-empty FAIL 1 "synthetic runtime is not empty" - -
	exit 1
fi

S3_ALIAS_OWNED=1
s3_intent install-container "$S3_ALIAS"
device_capture_timed 120 install.fixture env \
	TERMUX__PREFIX="$S3_PREFIX" TERMUX__HOME="$S3_HOME" \
	PD_PROOT_BIN="$S3_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
	proot-distro install --quiet --architecture aarch64 --name "$S3_ALIAS" "$archive"
if ((DEVICE_CAPTURE_RC == 0)) && s3_pd list --quiet >"$S3_RAW_DIR/synthetic-containers.installed" \
	2>"$S3_RAW_DIR/synthetic-containers.installed.stderr" && \
	grep -Fx -- "$S3_ALIAS" "$S3_RAW_DIR/synthetic-containers.installed" >/dev/null; then
	device_result install.fixture PASS 0 "fixture installed only in synthetic runtime" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result install.fixture FAIL "$DEVICE_CAPTURE_RC" "isolated fixture install failed" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	exit 1
fi

mkdir -m 0700 -p -- "$S3_SESSIONS"
if [[ -L $S3_SESSIONS || $(realpath -e -- "$S3_SESSIONS" 2>/dev/null || true) != "$S3_SESSIONS" ]]; then
	device_result preflight.sessions FAIL 1 "sessions directory is not exact sandbox path" - -
	exit 1
fi
S3_SESSIONS_ID=$(stat -c '%d:%i' -- "$S3_SESSIONS") || exit 1
device_result preflight.sessions PASS 0 "sessions directory identity pinned" - -

# C1: TERM reaches a cooperative root -> child -> grandchild tree.
c1_ok=1
if s3_launch C1-cooperate c1-cooperate cooperate; then
	c1_pid=$S3_LAST_PID
	if ! s3_engine_kill C1-cooperate "$c1_pid" TERM; then c1_ok=0; fi
	for role in root child grandchild; do
		if ! awk -F '\t' -v wanted_role="$role" \
			'$2 == "TERM" && $4 == wanted_role { found = 1 } END { exit found ? 0 : 1 }' \
			"$S3_RAW_DIR/control/c1-cooperate/events"; then
			c1_ok=0
		fi
	done
else
	c1_ok=0
fi
s3_result C1.cooperative "$c1_ok" \
	"exact session kill propagated TERM through the tree and drained it (${S3_LAST_KILL_MS}ms)"
((c1_ok)) || exit 1

# C2: direct PGID TERM is a negative control; exact engine kill escalates and
# also drains a grandchild that moved to a new session.
c2_ok=1
if s3_launch C2-escape c2-escape escape; then
	c2_pid=$S3_LAST_PID
	c2_root_key=$(s3_role_key "$c2_pid" root)
	c2_child_key=$(s3_role_key "$c2_pid" child)
	c2_grandchild_key=$(s3_role_key "$c2_pid" grandchild)
	if ((c2_ok)) && ! s3_authorize_group_term "$c2_pid" C2-escape; then c2_ok=0; fi
	if ((c2_ok)); then
		s3_intent negative-control-pgid "TERM:-$c2_pid"
		kill -TERM -- "-$c2_pid" 2>"$S3_RAW_DIR/C2-escape.pgid.stderr" || c2_ok=0
		sleep 0.5
	fi
	if ((c2_ok)) && { ! s3_root_identity_matches "$c2_pid" || \
		! s3_all_roles_alive "$c2_pid"; }; then c2_ok=0; fi
	if ((c2_ok)) && [[ ${S3_ROLE_PGID[$c2_grandchild_key]} == "$c2_pid" || \
		${S3_ROLE_SID[$c2_grandchild_key]} == "$c2_pid" ]]; then c2_ok=0; fi
	s3_capture_ps C2-escape.ps-after-pgid
	if ((DEVICE_CAPTURE_RC != 0)) || ! s3_ps_contains "$DEVICE_CAPTURE_STDOUT" "$c2_pid"; then
		c2_ok=0
	fi
	if ((c2_ok)) && ! s3_engine_kill C2-escape "$c2_pid" TERM; then c2_ok=0; fi
	if grep -F $'\tTERM\t' "$S3_RAW_DIR/control/c2-escape/events" >/dev/null 2>&1; then c2_ok=0; fi
	if ((c2_ok)) && ((S3_LAST_KILL_MS < 1800)); then c2_ok=0; fi
else
	c2_ok=0
fi
s3_result C2.escape-escalation "$c2_ok" \
	"PGID TERM was insufficient; exact session kill escalated and drained escaped tree (${S3_LAST_KILL_MS}ms)"
((c2_ok)) || exit 1

# C3: an exact session identifier scopes one of two sessions sharing an alias.
c3_ok=1
c3a_pid=
c3b_pid=
if s3_launch C3-a c3-a cooperate; then c3a_pid=$S3_LAST_PID; else c3_ok=0; fi
if ((c3_ok)) && s3_launch C3-b c3-b cooperate; then c3b_pid=$S3_LAST_PID; else c3_ok=0; fi
if ((c3_ok)) && [[ $c3a_pid == "$c3b_pid" ]]; then c3_ok=0; fi
if ((c3_ok)) && ! s3_engine_kill C3-a "$c3a_pid" TERM; then c3_ok=0; fi
if ((c3_ok)) && { ! s3_root_identity_matches "$c3b_pid" || \
	! s3_all_roles_alive "$c3b_pid" || ! s3_record_identity_matches "$c3b_pid"; }; then
	c3_ok=0
fi
if ((c3_ok)); then
	s3_capture_ps C3-b.ps-after-a
	if ((DEVICE_CAPTURE_RC != 0)) || ! s3_ps_contains "$DEVICE_CAPTURE_STDOUT" "$c3b_pid" || \
		s3_ps_contains "$DEVICE_CAPTURE_STDOUT" "$c3a_pid"; then c3_ok=0; fi
fi
if ((c3_ok)) && ! s3_engine_kill C3-b "$c3b_pid" TERM; then c3_ok=0; fi
s3_result C3.exact-scope "$c3_ok" \
	"exact session PID stopped A while same-alias session B remained intact"
((c3_ok)) || exit 1

# C4: after the tracer PRoot dies, inherited registry holders identify the
# surviving guests and the old exact session identifier remains killable.
c4_ok=1
if s3_launch C4-dead-proot c4-dead-proot orphan; then
	c4_pid=$S3_LAST_PID
	if ! s3_root_identity_matches "$c4_pid" || \
		[[ $(<"/proc/$c4_pid/comm") != proot ]]; then c4_ok=0; fi
	if ((c4_ok)); then
		s3_intent host-sigkill-root "$c4_pid:${S3_ROOT_STARTTIME[$c4_pid]}"
		kill -KILL "$c4_pid" 2>"$S3_RAW_DIR/C4-dead-proot.sigkill.stderr" || c4_ok=0
	fi
	c4_root_rc=0
	for ((c4_iteration = 0; c4_iteration < 50; c4_iteration += 1)); do
		s3_root_identity_matches "$c4_pid"
		c4_root_rc=$?
		if ((c4_root_rc == 1)); then break; fi
		if ((c4_root_rc != 0)); then c4_ok=0; break; fi
		sleep 0.1
	done
	if ((c4_root_rc != 1)); then c4_ok=0; fi
	if ((c4_ok)) && ! s3_reap_child_if_dead "$c4_pid"; then c4_ok=0; fi
	if ((c4_ok)) && ! s3_all_roles_alive "$c4_pid"; then c4_ok=0; fi
	if ((c4_ok)) && ! s3_holders_capture "$c4_pid" C4-dead-proot.orphan; then c4_ok=0; fi
	if ((c4_ok)); then
		for role in root child grandchild; do
			c4_key=$(s3_role_key "$c4_pid" "$role")
			grep -Fx -- "${S3_ROLE_PID[$c4_key]}" \
				"$S3_RAW_DIR/C4-dead-proot.orphan.holders" >/dev/null || c4_ok=0
		done
	fi
	s3_capture_ps C4-dead-proot.ps-orphan
	if ((DEVICE_CAPTURE_RC != 0)) || ! s3_ps_contains "$DEVICE_CAPTURE_STDOUT" "$c4_pid"; then
		c4_ok=0
	fi
	if ((c4_ok)) && ! s3_engine_kill C4-dead-proot "$c4_pid" TERM; then c4_ok=0; fi
else
	c4_ok=0
fi
s3_result C4.dead-proot "$c4_ok" \
	"old session PID found inherited holders after tracer SIGKILL and drained them (${S3_LAST_KILL_MS}ms)"
((c4_ok)) || exit 1

# Reliability is exercised only on the simplest qualified path.
stress_ok=1
stress_completed=0
for ((stress_iteration = 1; stress_iteration <= stress_cycles; stress_iteration += 1)); do
	printf -v stress_id 'stress-%03d' "$stress_iteration"
	if ! s3_launch "$stress_id" "$stress_id" cooperate; then
		stress_ok=0
		break
	fi
	stress_pid=$S3_LAST_PID
	if ! s3_engine_kill "$stress_id" "$stress_pid" TERM; then
		stress_ok=0
		break
	fi
	stress_completed=$stress_iteration
done
if ((stress_cycles == 0)); then
	device_result C5.stress SKIP - "stress loop disabled for this diagnostic run" - -
else
	s3_result C5.stress "$stress_ok" \
		"$stress_completed/$stress_cycles cooperative exact-session start/stop cycles drained without survivors"
	((stress_ok)) || exit 1
fi

s3_cleanup
if ((S3_CLEANUP_FAILURES == 0)); then
	device_result cleanup.objects PASS 0 "owned alias, sessions and sandbox removed" - -
else
	device_result cleanup.objects FAIL 1 "cleanup incomplete; sandbox preserved" - -
fi

device_finish
exit "$DEVICE_FAILURE_COUNT"
