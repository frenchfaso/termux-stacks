#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
FIXTURE_DIR=$SCRIPT_DIR/fixtures/s2

DEVICE_PHASE=S2
DEVICE_RUN_LABEL=termux-stacks-s2
DEVICE_RUNTIME_LABEL=txs-s2
DEVICE_HARNESS_VERSION=1
DEVICE_AUTOMATIC_SCOPE=$'The harness used a private TERMUX__PREFIX and one disposable exact-name alias.\nIt compared proot-distro 5.6.0 ps output with an independent child/process\noracle for T1, T2 and the F1-F3 registry faults. Signal/tree-kill policy,\ndaemon recovery and production parsing remain S3/S5.'

# shellcheck source=tests/device/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
	cat <<'EOF'
Usage:
  bash tests/device/s2.sh --archive ABSOLUTE_OCI_ARCHIVE \
    --archive-sha256 LOWERCASE_SHA256 [--output-root ABSOLUTE_DIR]

S2 installs the verified arm64 fixture only inside a synthetic
TERMUX__PREFIX owned by this run. It never targets the real proot-distro
runtime, clear-cache, reset, remove --all, or an existing alias.
EOF
}

archive=
archive_sha256=
output_root=
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

device_init "$output_root" || exit $?

S2_RAW_DIR=$DEVICE_EVIDENCE_DIR/registry
S2_ORACLE_FILE=$DEVICE_EVIDENCE_DIR/oracle.tsv
S2_GOLDEN_FILE=$DEVICE_EVIDENCE_DIR/golden.tsv
S2_INTENT_FILE=$DEVICE_EVIDENCE_DIR/intent.tsv
S2_CLEANUP_FILE=$DEVICE_EVIDENCE_DIR/cleanup.raw
S2_REAL_PRE=$DEVICE_EVIDENCE_DIR/real-containers.pre
S2_REAL_POST=$DEVICE_EVIDENCE_DIR/real-containers.post
mkdir -m 0700 -- "$S2_RAW_DIR"
printf 'phase\tpid\tstarttime\tppid\tpgid\tsid\tcomm\tboot_id\n' >"$S2_ORACLE_FILE"
printf 'case\tphase\toracle_alive\tregistry_contains_pid\texpected\n' >"$S2_GOLDEN_FILE"
printf 'time_utc\taction\ttarget\n' >"$S2_INTENT_FILE"
: >"$S2_CLEANUP_FILE"

run_id=$(printf '%x%04x' "$(date +%s)" "$RANDOM")
S2_ALIAS=txs-s2-$run_id-worker
S2_SANDBOX=$DEVICE_RUN_DIR/sandbox
S2_PREFIX=$S2_SANDBOX/prefix
S2_HOME=$S2_SANDBOX/home
S2_CONTROL=$S2_SANDBOX/control
S2_SENTINEL=$S2_SANDBOX/.termux-stacks-s2-sentinel
S2_SENTINEL_VALUE=$run_id-$RANDOM-$RANDOM
S2_SESSIONS=$S2_PREFIX/var/lib/proot-distro/sessions
S2_SANDBOX_ID=
S2_SESSIONS_ID=
S2_REAL_PREFIX=${PREFIX:-}
S2_BOOT_ID=
S2_CLEANUP_STATE=disabled
S2_CLEANUP_FAILURES=0
S2_PRESERVE_SANDBOX=0
S2_CONTAINMENT_PROVEN=0
S2_ALIAS_OWNED=0
S2_DEFERRED_SIGNAL=0
S2_LAUNCHED_PID=
S2_LAST_PS_RC=0
S2_LAST_PS_STDOUT=
S2_LAST_PS_STDERR=
S2_FAULT_SUCCESSES=0
S2_LAST_WAIT_RC=0

declare -a S2_PIDS=()
declare -A S2_TOKEN=()
declare -A S2_STARTTIME=()
declare -A S2_PGID=()
declare -A S2_SID=()
declare -A S2_ACTIVE=()
declare -A S2_SCOPE_CALIBRATED=()

s2_defer_signal() {
	local code=$1
	if ((S2_DEFERRED_SIGNAL == 0)); then
		S2_DEFERRED_SIGNAL=$code
	fi
}

s2_install_deferred_signal_handlers() {
	trap 's2_defer_signal 129' HUP
	trap 's2_defer_signal 130' INT
	trap 's2_defer_signal 143' TERM
}

s2_intent() {
	printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$2" \
		>>"$S2_INTENT_FILE"
}

s2_pd() {
	timeout --signal=KILL 15 env \
		TERMUX__PREFIX="$S2_PREFIX" \
		TERMUX__HOME="$S2_HOME" \
		PD_PROOT_BIN="$S2_REAL_PREFIX/bin/proot" \
		PD_FORCE_NO_COLORS=true \
		COLUMNS=240 \
		proot-distro "$@"
}

s2_proc_fields() {
	local pid=$1
	local line rest
	local -a fields
	[[ -r /proc/$pid/stat ]] || return 1
	IFS= read -r line <"/proc/$pid/stat" || return 1
	[[ $line == *') '* ]] || return 1
	rest=${line##*) }
	read -r -a fields <<<"$rest"
	((${#fields[@]} >= 20)) || return 1
	printf '%s\t%s\t%s\t%s\t%s\n' \
		"${fields[19]}" "${fields[1]}" "${fields[2]}" "${fields[3]}" "${fields[0]}"
}

s2_record_oracle() {
	local phase=$1
	local pid=$2
	local fields starttime ppid pgid sid state comm
	fields=$(s2_proc_fields "$pid") || return 1
	IFS=$'\t' read -r starttime ppid pgid sid state <<<"$fields"
	[[ $state != Z && $state != X ]] || return 1
	IFS= read -r comm <"/proc/$pid/comm" || return 1
	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
		"$phase" "$pid" "$starttime" "$ppid" "$pgid" "$sid" "$comm" "$S2_BOOT_ID" \
		>>"$S2_ORACLE_FILE"
	printf '%s\t%s\t%s\n' "$starttime" "$pgid" "$sid"
}

s2_identity_matches() {
	local pid=$1
	local fields starttime _ppid pgid sid state boot_now
	[[ ${S2_STARTTIME[$pid]+set} ]] || return 1
	boot_now=$(< /proc/sys/kernel/random/boot_id) || return 1
	[[ $boot_now == "$S2_BOOT_ID" ]] || return 1
	fields=$(s2_proc_fields "$pid") || return 1
	IFS=$'\t' read -r starttime _ppid pgid sid state <<<"$fields"
	[[ $starttime == "${S2_STARTTIME[$pid]}" && \
		$pgid == "${S2_PGID[$pid]}" && $sid == "${S2_SID[$pid]}" && \
		$state != Z && $state != X ]]
}

s2_pid_alive() {
	s2_identity_matches "$1"
}

s2_raw_pid_exists() {
	[[ $1 =~ ^[1-9][0-9]*$ && -r /proc/$1/stat ]]
}

s2_sessions_identity_matches() {
	local canonical current_id
	[[ -n $S2_SESSIONS_ID && -d $S2_SESSIONS && ! -L $S2_SESSIONS ]] || return 1
	canonical=$(realpath -e -- "$S2_SESSIONS" 2>/dev/null) || return 1
	current_id=$(stat -c '%d:%i' -- "$S2_SESSIONS" 2>/dev/null) || return 1
	[[ $canonical == "$S2_SESSIONS" && $current_id == "$S2_SESSIONS_ID" ]]
}

s2_scope_is_empty() {
	local pid=$1
	local pgid=${S2_PGID[$pid]:-}
	local sid=${S2_SID[$pid]:-}
	local snapshot=$S2_RAW_DIR/scope-$pid.raw
	local scan_rc
	[[ $pgid =~ ^[1-9][0-9]*$ && $sid =~ ^[1-9][0-9]*$ && \
		${S2_SCOPE_CALIBRATED[$pid]:-0} == 1 ]] || return 2
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 2
	awk -v group="$pgid" -v session="$sid" \
		'$2 == group || $3 == session { found = 1 } END { exit found ? 0 : 1 }' \
		"$snapshot"
	scan_rc=$?
	case $scan_rc in
		0) return 1 ;;
		1) return 0 ;;
		*) return 2 ;;
	esac
}

s2_calibrate_scope_oracle() {
	local pid=$1
	local pgid=${S2_PGID[$pid]:-}
	local sid=${S2_SID[$pid]:-}
	local snapshot=$S2_RAW_DIR/scope-calibration-$pid.raw
	local scan_rc
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 1
	awk -v wanted_pid="$pid" -v wanted_pgid="$pgid" -v wanted_sid="$sid" \
		'$1 == wanted_pid && $2 == wanted_pgid && $3 == wanted_sid { found = 1 }
		END { exit found ? 0 : 1 }' "$snapshot"
	scan_rc=$?
	[[ $scan_rc == 0 ]] || return 1
	S2_SCOPE_CALIBRATED[$pid]=1
}

s2_group_is_owned() {
	local pid=$1
	local pgid=${S2_PGID[$pid]:-}
	local sid=${S2_SID[$pid]:-}
	local snapshot=$S2_RAW_DIR/cleanup-group-$pid.raw
	[[ $pgid =~ ^[0-9]+$ && $sid =~ ^[0-9]+$ ]] || return 1
	ps -A -o pid=,pgid=,sid=,comm= >"$snapshot" 2>&1 || return 1
	awk -v group="$pgid" -v session="$sid" '
		$2 == group { found = 1; if ($3 != session) bad = 1 }
		END { exit !(found && !bad) }
	' "$snapshot"
}

s2_wait_scope_empty() {
	local pid=$1
	local max_iterations=${2:-60}
	local iteration
	local reaped=0
	local wait_rc=0
	for ((iteration = 0; iteration < max_iterations; iteration += 1)); do
		if ! s2_pid_alive "$pid"; then
			if ((reaped == 0)); then
				wait "$pid" 2>/dev/null || wait_rc=$?
				reaped=1
				S2_LAST_WAIT_RC=$wait_rc
			fi
			if s2_scope_is_empty "$pid"; then
				return 0
			fi
		fi
		sleep 0.1
	done
	return 1
}

s2_stop_session() {
	local pid=$1
	local token=${S2_TOKEN[$pid]:-}
	local rc=0
	[[ ${S2_ACTIVE[$pid]:-0} == 1 ]] || return 0
	if [[ -n $token ]]; then
		: >"$S2_CONTROL/$token.stop" 2>/dev/null || true
	fi
	S2_LAST_WAIT_RC=0
	if ! s2_wait_scope_empty "$pid" 60; then
		return 1
	fi
	rc=$S2_LAST_WAIT_RC
	S2_ACTIVE[$pid]=0
	return "$rc"
}

s2_force_cleanup_session() {
	local pid=$1
	local iteration
	[[ ${S2_ACTIVE[$pid]:-0} == 1 ]] || return 0
	: >"$S2_CONTROL/${S2_TOKEN[$pid]}.stop" 2>/dev/null || true
	if [[ ! ${S2_STARTTIME[$pid]+set} ]]; then
		printf 'UNQUALIFIED_CHILD\t%s\twaiting for control/TTL; no signal authorized\n' "$pid" \
			>>"$S2_CLEANUP_FILE"
		for ((iteration = 0; iteration < 650; iteration += 1)); do
			sleep 0.1
		done
		if ! s2_raw_pid_exists "$pid"; then
			wait "$pid" 2>/dev/null || true
		fi
		return 1
	fi
	if s2_wait_scope_empty "$pid" 60; then
		S2_ACTIVE[$pid]=0
		return 0
	fi
	if ! s2_identity_matches "$pid" || ! s2_group_is_owned "$pid"; then
		printf 'AMBIGUOUS\tidentity/group not proven for pid %s; waiting for fixture TTL\n' "$pid" \
			>>"$S2_CLEANUP_FILE"
		if s2_wait_scope_empty "$pid" 650; then
			S2_ACTIVE[$pid]=0
			return 0
		fi
		return 1
	fi
	printf 'TERM_GROUP\t%s\n' "${S2_PGID[$pid]}" >>"$S2_CLEANUP_FILE"
	kill -TERM -- "-${S2_PGID[$pid]}" 2>/dev/null || true
	if ! s2_wait_scope_empty "$pid" 20; then
		if ! s2_identity_matches "$pid" || ! s2_group_is_owned "$pid"; then
			if s2_wait_scope_empty "$pid" 650; then
				S2_ACTIVE[$pid]=0
				return 0
			fi
			return 1
		fi
		printf 'KILL_GROUP\t%s\n' "${S2_PGID[$pid]}" >>"$S2_CLEANUP_FILE"
		kill -KILL -- "-${S2_PGID[$pid]}" 2>/dev/null || true
		s2_wait_scope_empty "$pid" 20 || return 1
	fi
	S2_ACTIVE[$pid]=0
}

s2_capture_ps() {
	local capture_id=$1
	local quiet=$2
	if [[ $quiet == quiet ]]; then
		device_capture_timed 15 "$capture_id" env \
			TERMUX__PREFIX="$S2_PREFIX" TERMUX__HOME="$S2_HOME" \
			PD_PROOT_BIN="$S2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
			COLUMNS=240 proot-distro ps --quiet
	else
		device_capture_timed 15 "$capture_id" env \
			TERMUX__PREFIX="$S2_PREFIX" TERMUX__HOME="$S2_HOME" \
			PD_PROOT_BIN="$S2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
			COLUMNS=240 proot-distro ps
	fi
	S2_LAST_PS_RC=$DEVICE_CAPTURE_RC
	S2_LAST_PS_STDOUT=$DEVICE_CAPTURE_STDOUT
	S2_LAST_PS_STDERR=$DEVICE_CAPTURE_STDERR
}

s2_ps_contains() {
	grep -Fx -- "$2" "$1" >/dev/null 2>&1
}

s2_golden() {
	printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" \
		>>"$S2_GOLDEN_FILE"
}

s2_launch() {
	local case_id=$1
	local token=$2
	local pid identity starttime pgid sid
	rm -f -- "$S2_CONTROL/$token.ready" "$S2_CONTROL/$token.stop" \
		"$S2_CONTROL/$token.events"
	s2_intent launch-session "$case_id:$token"
	setsid env \
		TERMUX__PREFIX="$S2_PREFIX" \
		TERMUX__HOME="$S2_HOME" \
		PD_PROOT_BIN="$S2_REAL_PREFIX/bin/proot" \
		PD_FORCE_NO_COLORS=true \
		COLUMNS=240 \
		proot-distro run --isolated \
		--bind "$S2_CONTROL:/control" \
		--env TSTACK_S2_CONTROL=/control \
		--env "TSTACK_S2_TOKEN=$token" \
		--env TSTACK_S2_TTL=60 \
		"$S2_ALIAS" \
		>"$S2_RAW_DIR/$case_id.run.stdout" \
		2>"$S2_RAW_DIR/$case_id.run.stderr" &
	pid=$!
	S2_PIDS+=("$pid")
	S2_TOKEN[$pid]=$token
	S2_ACTIVE[$pid]=1
	local iteration
	for ((iteration = 0; iteration < 50; iteration += 1)); do
		[[ -f $S2_CONTROL/$token.ready ]] && break
		kill -0 "$pid" 2>/dev/null || break
		sleep 0.1
	done
	if [[ ! -f $S2_CONTROL/$token.ready ]] || ! kill -0 "$pid" 2>/dev/null; then
		return 1
	fi
	identity=$(s2_record_oracle "$case_id.ready" "$pid") || return 1
	IFS=$'\t' read -r starttime pgid sid <<<"$identity"
	S2_STARTTIME[$pid]=$starttime
	S2_PGID[$pid]=$pgid
	S2_SID[$pid]=$sid
	if [[ $pgid != "$pid" || $sid != "$pid" ]]; then
		return 1
	fi
	s2_calibrate_scope_oracle "$pid" || return 1
	S2_LAUNCHED_PID=$pid
}

s2_record_case() {
	local id=$1
	local ok=$2
	local detail=$3
	if ((ok)); then
		device_result "$id" PASS 0 "$detail" - -
	else
		device_result "$id" FAIL 1 "$detail" - -
	fi
}

s2_cleanup() {
	local pid current canonical id_now
	[[ $S2_CLEANUP_STATE == pending ]] || return 0
	s2_install_deferred_signal_handlers
	S2_CLEANUP_STATE=running
	printf 'cleanup_started\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$S2_CLEANUP_FILE"
	if [[ -n $S2_SESSIONS_ID ]]; then
		if s2_sessions_identity_matches; then
			chmod 0700 "$S2_SESSIONS" 2>/dev/null || true
		else
			printf 'AMBIGUOUS\tsessions directory identity changed; mode not modified\n' \
				>>"$S2_CLEANUP_FILE"
			S2_CLEANUP_FAILURES=$((S2_CLEANUP_FAILURES + 1))
			S2_PRESERVE_SANDBOX=1
		fi
	fi
	for pid in "${S2_PIDS[@]}"; do
		if ! s2_force_cleanup_session "$pid"; then
			printf 'LIVE_OR_AMBIGUOUS\t%s\n' "$pid" >>"$S2_CLEANUP_FILE"
			S2_CLEANUP_FAILURES=$((S2_CLEANUP_FAILURES + 1))
			S2_PRESERVE_SANDBOX=1
		fi
	done
	if ((S2_CLEANUP_FAILURES == 0 && S2_PRESERVE_SANDBOX == 0 && \
		S2_CONTAINMENT_PROVEN == 1 && S2_ALIAS_OWNED == 1)); then
		if s2_pd list --quiet >"$DEVICE_RUNTIME_DIR/s2-containers.current" \
			2>>"$S2_CLEANUP_FILE"; then
			if grep -Fx -- "$S2_ALIAS" "$DEVICE_RUNTIME_DIR/s2-containers.current" >/dev/null; then
				s2_intent remove-container "$S2_ALIAS"
				s2_pd remove --quiet "$S2_ALIAS" >>"$S2_CLEANUP_FILE" 2>&1 || \
					S2_CLEANUP_FAILURES=$((S2_CLEANUP_FAILURES + 1))
			fi
		else
			S2_CLEANUP_FAILURES=$((S2_CLEANUP_FAILURES + 1))
		fi
	fi
	if ((S2_CLEANUP_FAILURES == 0 && S2_PRESERVE_SANDBOX == 0)); then
		canonical=$(realpath -e -- "$S2_SANDBOX" 2>/dev/null || true)
		id_now=$(stat -c '%d:%i' -- "$S2_SANDBOX" 2>/dev/null || true)
		if [[ $canonical == "$S2_SANDBOX" && ! -L $S2_SANDBOX && \
			$id_now == "$S2_SANDBOX_ID" && -f $S2_SENTINEL ]] && \
			[[ $(<"$S2_SENTINEL") == "$S2_SENTINEL_VALUE" ]]; then
			rm -rf -- "$S2_SANDBOX"
		else
			printf 'AMBIGUOUS\tsandbox identity changed; not removed\n' >>"$S2_CLEANUP_FILE"
			S2_CLEANUP_FAILURES=$((S2_CLEANUP_FAILURES + 1))
			S2_PRESERVE_SANDBOX=1
		fi
	else
		S2_PRESERVE_SANDBOX=1
	fi
	env PD_FORCE_NO_COLORS=true proot-distro list --quiet >"$S2_REAL_POST" 2>&1 || \
		S2_CLEANUP_FAILURES=$((S2_CLEANUP_FAILURES + 1))
	if grep -Fx -- "$S2_ALIAS" "$S2_REAL_POST" >/dev/null 2>&1; then
		printf 'REAL_RUNTIME_COLLISION\t%s\n' "$S2_ALIAS" >>"$S2_CLEANUP_FILE"
		S2_CLEANUP_FAILURES=$((S2_CLEANUP_FAILURES + 1))
	fi
	printf 'cleanup_finished\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$S2_CLEANUP_FILE"
	S2_CLEANUP_STATE=done
}

s2_on_exit() {
	local original_rc=$1
	local was_pending=0
	s2_install_deferred_signal_handlers
	trap - EXIT
	[[ $S2_CLEANUP_STATE == pending ]] && was_pending=1
	s2_cleanup
	if ((was_pending)) && ((DEVICE_FINISHED == 0)); then
		if ((S2_CLEANUP_FAILURES == 0)); then
			device_result cleanup.objects PASS 0 "owned sessions, alias and sandbox removed" - -
		else
			device_result cleanup.objects FAIL 1 "cleanup incomplete; sandbox preserved if needed" - -
		fi
	fi
	if ((DEVICE_FINISHED == 0)); then
		device_finish || true
	fi
	device_cleanup
	if ((S2_CLEANUP_FAILURES > 0 || DEVICE_FAILURE_COUNT > 0)) && ((original_rc == 0)); then
		original_rc=1
	fi
	if ((S2_DEFERRED_SIGNAL > 0)); then
		original_rc=$S2_DEFERRED_SIGNAL
	fi
	trap - HUP INT TERM
	exit "$original_rc"
}

trap 's2_on_exit $?' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

preflight_failed=0
for command_name in proot-distro proot timeout sha256sum tar jq setsid ps stat \
	realpath flock dpkg-query awk grep chmod find cp; do
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
if [[ ! -x $FIXTURE_DIR/verify-oci.sh ]]; then
	device_result preflight.validator FAIL 2 "OCI validator is missing or not executable" - -
	preflight_failed=1
fi
if [[ -z $S2_REAL_PREFIX || $S2_REAL_PREFIX != /* || ! -x $S2_REAL_PREFIX/bin/proot ]]; then
	device_result preflight.prefix FAIL 2 "canonical Termux PREFIX/proot is unavailable" - -
	preflight_failed=1
fi
if ((preflight_failed)); then
	device_finish
	exit 1
fi

archive_source=$archive
private_archive=$DEVICE_WORK_DIR/s2-fixture.oci.tar
if ! cp -- "$archive_source" "$private_archive" || ! chmod 0600 "$private_archive"; then
	device_result preflight.archive-copy FAIL 1 "cannot snapshot archive into private run storage" - -
	device_finish
	exit 1
fi
archive=$private_archive
fixture_worker_sha=$(sha256sum "$FIXTURE_DIR/worker" | awk '{print $1}')
device_capture_timed 30 preflight.oci "$FIXTURE_DIR/verify-oci.sh" \
	"$archive" "$archive_sha256" "$fixture_worker_sha"
if ((DEVICE_CAPTURE_RC == 0)); then
	device_result preflight.oci PASS 0 "OCI archive, platform and blobs verified" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result preflight.oci FAIL "$DEVICE_CAPTURE_RC" "OCI validation failed" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	device_finish
	exit 1
fi

if [[ $(uname -m) != aarch64 || $(dpkg --print-architecture 2>/dev/null) != aarch64 ]]; then
	device_result preflight.architecture FAIL 1 "S2 requires native aarch64 Termux" - -
	device_finish
	exit 1
fi
device_result preflight.architecture PASS 0 "native aarch64 confirmed" - -

pd_version=$(dpkg-query -W -f='${Version}' proot-distro 2>/dev/null || true)
if [[ $pd_version != 5.6.0 ]]; then
	device_result preflight.engine FAIL 1 "S2 is pinned to proot-distro 5.6.0; found $pd_version" - -
	device_finish
	exit 1
fi
device_result preflight.engine PASS 0 "proot-distro 5.6.0 confirmed" - -

S2_BOOT_ID=$(< /proc/sys/kernel/random/boot_id) || {
	device_result preflight.proc FAIL 1 "boot_id is unreadable" - -
	device_finish
	exit 1
}
device_metadata run_id "$run_id"
device_metadata archive_source "$archive_source"
device_metadata archive_sha256 "$archive_sha256"
device_metadata harness_sha256 "$(sha256sum "$SCRIPT_DIR/s2.sh" | awk '{print $1}')"
device_metadata shared_lib_sha256 "$(sha256sum "$SCRIPT_DIR/lib.sh" | awk '{print $1}')"
device_metadata validator_sha256 "$(sha256sum "$FIXTURE_DIR/verify-oci.sh" | awk '{print $1}')"
device_metadata worker_sha256 "$fixture_worker_sha"
device_metadata proot_distro_version "$pd_version"
device_metadata boot_id "$S2_BOOT_ID"
device_metadata architecture "$(uname -m)"
device_metadata android_sdk "$(getprop ro.build.version.sdk 2>/dev/null || printf unavailable)"

mkdir -m 0700 -- "$S2_SANDBOX" "$S2_PREFIX" "$S2_HOME" "$S2_CONTROL"
printf '%s\n' "$S2_SENTINEL_VALUE" >"$S2_SENTINEL"
S2_SANDBOX=$(realpath -e -- "$S2_SANDBOX") || exit 1
S2_PREFIX=$S2_SANDBOX/prefix
S2_HOME=$S2_SANDBOX/home
S2_CONTROL=$S2_SANDBOX/control
S2_SENTINEL=$S2_SANDBOX/.termux-stacks-s2-sentinel
S2_SESSIONS=$S2_PREFIX/var/lib/proot-distro/sessions
S2_SANDBOX_ID=$(stat -c '%d:%i' -- "$S2_SANDBOX") || exit 1
S2_CLEANUP_STATE=pending

env PD_FORCE_NO_COLORS=true proot-distro list --quiet >"$S2_REAL_PRE" 2>&1 || {
	device_result preflight.real-runtime FAIL 1 "real container inventory failed" - -
	exit 1
}
if grep -Fx -- "$S2_ALIAS" "$S2_REAL_PRE" >/dev/null; then
	device_result preflight.real-runtime FAIL 1 "random alias unexpectedly exists in real runtime" - -
	exit 1
fi
device_result preflight.real-runtime PASS 0 "exact alias absent from real runtime" - -

device_capture_timed 15 preflight.synthetic-help env \
	TERMUX__PREFIX="$S2_PREFIX" TERMUX__HOME="$S2_HOME" \
	PD_PROOT_BIN="$S2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
	proot-distro help
if ((DEVICE_CAPTURE_RC == 0)) && \
	grep -F -- "$S2_PREFIX/var/lib/proot-distro" \
		"$DEVICE_CAPTURE_STDOUT" "$DEVICE_CAPTURE_STDERR" >/dev/null; then
	device_result preflight.synthetic-help PASS 0 "engine data location is contained in sandbox" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	S2_CONTAINMENT_PROVEN=1
else
	device_result preflight.synthetic-help FAIL "$DEVICE_CAPTURE_RC" \
		"cannot prove synthetic engine data location" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	exit 1
fi

if s2_pd list --quiet >"$S2_RAW_DIR/synthetic-containers.pre" 2>"$S2_RAW_DIR/synthetic-containers.pre.stderr" && \
	[[ ! -s $S2_RAW_DIR/synthetic-containers.pre ]]; then
	device_result preflight.synthetic-empty PASS 0 "synthetic runtime starts empty" - -
else
	device_result preflight.synthetic-empty FAIL 1 "synthetic runtime is not provably empty" - -
	exit 1
fi

S2_ALIAS_OWNED=1
s2_intent install-container "$S2_ALIAS"
device_capture_timed 120 install.fixture env \
	TERMUX__PREFIX="$S2_PREFIX" TERMUX__HOME="$S2_HOME" \
	PD_PROOT_BIN="$S2_REAL_PREFIX/bin/proot" PD_FORCE_NO_COLORS=true \
	proot-distro install --quiet --architecture aarch64 --name "$S2_ALIAS" "$archive"
if ((DEVICE_CAPTURE_RC == 0)) && s2_pd list --quiet >"$S2_RAW_DIR/synthetic-containers.installed" \
	2>"$S2_RAW_DIR/synthetic-containers.installed.stderr" && \
	grep -Fx -- "$S2_ALIAS" "$S2_RAW_DIR/synthetic-containers.installed" >/dev/null; then
	device_result install.fixture PASS 0 "fixture installed only in synthetic runtime" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result install.fixture FAIL "$DEVICE_CAPTURE_RC" "isolated fixture install failed" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	exit 1
fi

mkdir -m 0700 -p -- "$S2_SESSIONS"
if [[ -L $S2_SESSIONS || $(realpath -e -- "$S2_SESSIONS" 2>/dev/null || true) != "$S2_SESSIONS" ]]; then
	device_result preflight.sessions FAIL 1 "sessions directory is not the exact sandbox path" - -
	exit 1
fi
S2_SESSIONS_ID=$(stat -c '%d:%i' -- "$S2_SESSIONS") || exit 1
device_result preflight.sessions PASS 0 "sessions directory canonical identity pinned" - -

# T1: one visible session, normal exit and stale-record pruning.
t1_ok=1
if s2_launch T1 t1; then
	t1_pid=$S2_LAUNCHED_PID
	s2_capture_ps T1.quiet-live quiet
	if ((S2_LAST_PS_RC != 0)) || ! s2_ps_contains "$S2_LAST_PS_STDOUT" "$t1_pid"; then t1_ok=0; fi
	s2_golden T1 live true "$(s2_ps_contains "$S2_LAST_PS_STDOUT" "$t1_pid" && printf true || printf false)" present
	s2_capture_ps T1.full-live full
	if ((S2_LAST_PS_RC != 0)) || ! grep -F -- "$t1_pid" "$S2_LAST_PS_STDOUT" "$S2_LAST_PS_STDERR" >/dev/null; then t1_ok=0; fi
	if ! s2_stop_session "$t1_pid"; then t1_ok=0; fi
	s2_capture_ps T1.quiet-exit quiet
	if ((S2_LAST_PS_RC != 0)) || s2_ps_contains "$S2_LAST_PS_STDOUT" "$t1_pid"; then t1_ok=0; fi
	s2_golden T1 exit false "$(s2_ps_contains "$S2_LAST_PS_STDOUT" "$t1_pid" && printf true || printf false)" absent
else
	t1_ok=0
fi
s2_record_case T1.single "$t1_ok" "single session visibility, raw table and normal pruning"
((t1_ok)) || exit 1

# T2: two concurrent sessions on one alias are independently represented.
t2_ok=1
if s2_launch T2a t2a; then t2a_pid=$S2_LAUNCHED_PID; else t2_ok=0; fi
if ((t2_ok)) && s2_launch T2b t2b; then t2b_pid=$S2_LAUNCHED_PID; else t2_ok=0; fi
if ((t2_ok)); then
	s2_capture_ps T2.quiet-both quiet
	if ((S2_LAST_PS_RC != 0)) || ! s2_ps_contains "$S2_LAST_PS_STDOUT" "$t2a_pid" || \
		! s2_ps_contains "$S2_LAST_PS_STDOUT" "$t2b_pid" || [[ $t2a_pid == "$t2b_pid" ]]; then t2_ok=0; fi
	if ! s2_stop_session "$t2a_pid"; then t2_ok=0; fi
	s2_capture_ps T2.quiet-one quiet
	if ((S2_LAST_PS_RC != 0)) || s2_ps_contains "$S2_LAST_PS_STDOUT" "$t2a_pid" || \
		! s2_ps_contains "$S2_LAST_PS_STDOUT" "$t2b_pid"; then t2_ok=0; fi
	if ! s2_stop_session "$t2b_pid"; then t2_ok=0; fi
	s2_capture_ps T2.quiet-none quiet
	if ((S2_LAST_PS_RC != 0)) || s2_ps_contains "$S2_LAST_PS_STDOUT" "$t2a_pid" || \
		s2_ps_contains "$S2_LAST_PS_STDOUT" "$t2b_pid"; then t2_ok=0; fi
	s2_golden T2 both-to-one-to-none false false "independent records"
fi
s2_record_case T2.same-alias "$t2_ok" "two sessions on one alias are independently visible and pruned"
((t2_ok)) || exit 1

# F1: write denial makes registration fail silently while the workload runs.
f1_ok=1
if ! s2_sessions_identity_matches; then f1_ok=0; S2_PRESERVE_SANDBOX=1; fi
if ((f1_ok)); then chmod 0500 "$S2_SESSIONS" || f1_ok=0; fi
if touch "$S2_SESSIONS/.write-probe" >/dev/null 2>&1; then
	rm -f -- "$S2_SESSIONS/.write-probe"
	f1_ok=0
fi
if ((f1_ok)) && s2_launch F1 f1; then
	f1_pid=$S2_LAUNCHED_PID
	s2_capture_ps F1.quiet-denied quiet
	if ((S2_LAST_PS_RC != 0)) || [[ -s $S2_LAST_PS_STDOUT ]] || \
		s2_ps_contains "$S2_LAST_PS_STDOUT" "$f1_pid" || ! s2_pid_alive "$f1_pid"; then f1_ok=0; fi
	s2_golden F1 registration-denied true "$(s2_ps_contains "$S2_LAST_PS_STDOUT" "$f1_pid" && printf true || printf false)" omitted
	if ! s2_sessions_identity_matches; then f1_ok=0; S2_PRESERVE_SANDBOX=1; fi
	if ((f1_ok)); then chmod 0700 "$S2_SESSIONS" || f1_ok=0; fi
	s2_capture_ps F1.quiet-restored quiet
	if ((S2_LAST_PS_RC != 0)) || s2_ps_contains "$S2_LAST_PS_STDOUT" "$f1_pid"; then f1_ok=0; fi
	if [[ -e $S2_SESSIONS/$f1_pid.json ]]; then f1_ok=0; fi
	if ! s2_stop_session "$f1_pid"; then f1_ok=0; fi
else
	f1_ok=0
	if s2_sessions_identity_matches; then chmod 0700 "$S2_SESSIONS" 2>/dev/null || true; fi
fi
s2_record_case F1.registration-denied "$f1_ok" "live workload omitted after best-effort registration failure"
if ((f1_ok)); then S2_FAULT_SUCCESSES=$((S2_FAULT_SUCCESSES + 1)); else exit 1; fi

# F2: read denial makes a previously visible live session disappear transiently.
f2_ok=1
if s2_launch F2 f2; then
	f2_pid=$S2_LAUNCHED_PID
	s2_capture_ps F2.quiet-before quiet
	if ((S2_LAST_PS_RC != 0)) || ! s2_ps_contains "$S2_LAST_PS_STDOUT" "$f2_pid"; then f2_ok=0; fi
	if ! s2_sessions_identity_matches; then f2_ok=0; S2_PRESERVE_SANDBOX=1; fi
	if ((f2_ok)); then chmod 0000 "$S2_SESSIONS" || f2_ok=0; fi
	if ls "$S2_SESSIONS" >/dev/null 2>&1; then f2_ok=0; fi
	s2_capture_ps F2.quiet-denied quiet
	if ((S2_LAST_PS_RC != 0)) || [[ -s $S2_LAST_PS_STDOUT ]] || \
		s2_ps_contains "$S2_LAST_PS_STDOUT" "$f2_pid" || ! s2_pid_alive "$f2_pid"; then f2_ok=0; fi
	s2_golden F2 read-denied true "$(s2_ps_contains "$S2_LAST_PS_STDOUT" "$f2_pid" && printf true || printf false)" omitted
	if ! s2_sessions_identity_matches; then f2_ok=0; S2_PRESERVE_SANDBOX=1; fi
	if ((f2_ok)); then chmod 0700 "$S2_SESSIONS" || f2_ok=0; fi
	s2_capture_ps F2.quiet-restored quiet
	if ((S2_LAST_PS_RC != 0)) || ! s2_ps_contains "$S2_LAST_PS_STDOUT" "$f2_pid"; then f2_ok=0; fi
	if ! s2_stop_session "$f2_pid"; then f2_ok=0; fi
else
	f2_ok=0
	if s2_sessions_identity_matches; then chmod 0700 "$S2_SESSIONS" 2>/dev/null || true; fi
fi
s2_record_case F2.reader-denied "$f2_ok" "live session disappears on read failure and reappears after restore"
if ((f2_ok)); then S2_FAULT_SUCCESSES=$((S2_FAULT_SUCCESSES + 1)); else exit 1; fi

# F3: malformed JSON remains locked/live but is omitted from ps.
f3_ok=1
if s2_launch F3 f3; then
	f3_pid=$S2_LAUNCHED_PID
	f3_record=$S2_SESSIONS/$f3_pid.json
	s2_capture_ps F3.quiet-before quiet
	if ! s2_sessions_identity_matches; then f3_ok=0; S2_PRESERVE_SANDBOX=1; fi
	if ((S2_LAST_PS_RC != 0)) || ! s2_ps_contains "$S2_LAST_PS_STDOUT" "$f3_pid" || \
		[[ ! -f $f3_record || -L $f3_record ]]; then f3_ok=0; fi
	f3_parent=$(realpath -e -- "$(dirname -- "$f3_record")" 2>/dev/null || true)
	f3_before=$(stat -c '%d:%i' -- "$f3_record" 2>/dev/null || true)
	if [[ $f3_parent != "$S2_SESSIONS" || -z $f3_before ]]; then f3_ok=0; fi
	if flock -n "$f3_record" true >/dev/null 2>&1; then f3_ok=0; fi
	s2_intent truncate-live-record "$f3_pid:$f3_before"
	if ! s2_sessions_identity_matches; then f3_ok=0; S2_PRESERVE_SANDBOX=1; fi
	if ((f3_ok)); then printf '{' >"$f3_record" || f3_ok=0; fi
	f3_after=$(stat -c '%d:%i' -- "$f3_record" 2>/dev/null || true)
	if [[ $f3_after != "$f3_before" ]] || jq -e . "$f3_record" >/dev/null 2>&1; then f3_ok=0; fi
	s2_capture_ps F3.quiet-corrupt quiet
	if ((S2_LAST_PS_RC != 0)) || [[ -s $S2_LAST_PS_STDOUT ]] || \
		s2_ps_contains "$S2_LAST_PS_STDOUT" "$f3_pid" || \
		! s2_pid_alive "$f3_pid" || [[ ! -e $f3_record ]]; then f3_ok=0; fi
	s2_golden F3 malformed-live true "$(s2_ps_contains "$S2_LAST_PS_STDOUT" "$f3_pid" && printf true || printf false)" omitted
	if ! s2_stop_session "$f3_pid"; then f3_ok=0; fi
	s2_capture_ps F3.quiet-exit quiet
	if ((S2_LAST_PS_RC != 0)) || s2_ps_contains "$S2_LAST_PS_STDOUT" "$f3_pid" || \
		[[ -e $f3_record ]]; then f3_ok=0; fi
else
	f3_ok=0
fi
s2_record_case F3.malformed-live "$f3_ok" "locked malformed record omits a live workload, then prunes after exit"
if ((f3_ok)); then S2_FAULT_SUCCESSES=$((S2_FAULT_SUCCESSES + 1)); else exit 1; fi

if ((S2_FAULT_SUCCESSES == 3)); then
	device_result decision.fail-closed PASS 0 \
		"ps empty is never sufficient proof of absence; post-handle state must be unknown" - -
else
	device_result decision.fail-closed FAIL 1 "fault corpus is incomplete" - -
fi

s2_cleanup
if ((S2_CLEANUP_FAILURES == 0)); then
	device_result cleanup.objects PASS 0 "owned sessions, exact alias and synthetic runtime removed" - -
else
	device_result cleanup.objects FAIL 1 "cleanup incomplete; sandbox preserved for review" - -
fi

device_finish || true
if ((S2_DEFERRED_SIGNAL > 0)); then
	exit "$S2_DEFERRED_SIGNAL"
elif ((DEVICE_FAILURE_COUNT > 0)); then
	exit 1
fi
exit 0
