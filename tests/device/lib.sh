#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

# Shared helpers for device harnesses. This file is sourced by s0.sh.

if [[ -z ${BASH_VERSION:-} ]]; then
	printf '%s\n' "device harness requires Bash" >&2
	return 2 2>/dev/null || exit 2
fi

export LC_ALL=C
export TZ=UTC
umask 077

DEVICE_HARNESS_VERSION=1
DEVICE_RUN_DIR=
DEVICE_WORK_DIR=
DEVICE_EVIDENCE_DIR=
DEVICE_STDIO_DIR=
DEVICE_METADATA_FILE=
DEVICE_RESULTS_FILE=
DEVICE_ACTIVE_DAEMON_PID=
DEVICE_FAILURE_COUNT=0
DEVICE_CAPTURE_RC=0
DEVICE_CAPTURE_STDOUT=
DEVICE_CAPTURE_STDERR=
DEVICE_FINISHED=0

device_error() {
	printf 'device-harness: %s\n' "$*" >&2
}

device_sanitize_tsv() {
	local value=${1-}
	value=${value//$'\t'/ }
	value=${value//$'\r'/ }
	value=${value//$'\n'/\\n}
	printf '%s' "$value"
}

device_require_result_id() {
	[[ ${1-} =~ ^[A-Za-z0-9][A-Za-z0-9_.-]*$ ]]
}

device_init() {
	local output_root=${1-}
	local canonical_root

	if [[ -z $output_root ]]; then
		output_root=${TMPDIR:-}
	fi
	if [[ -z $output_root ]]; then
		device_error "TMPDIR is unset; pass --output-root explicitly"
		return 2
	fi
	if [[ $output_root != /* ]]; then
		device_error "output root must be absolute: $output_root"
		return 2
	fi
	if [[ ! -d $output_root || ! -w $output_root ]]; then
		device_error "output root must be an existing writable directory: $output_root"
		return 2
	fi

	canonical_root=$(cd -- "$output_root" 2>/dev/null && pwd -P) || {
		device_error "cannot resolve output root: $output_root"
		return 2
	}
	DEVICE_RUN_DIR=$(mktemp -d "$canonical_root/termux-stacks-s0.XXXXXXXX") || {
		device_error "cannot create isolated run directory under $canonical_root"
		return 2
	}
	chmod 0700 "$DEVICE_RUN_DIR" || return 2

	DEVICE_WORK_DIR=$DEVICE_RUN_DIR/work
	DEVICE_EVIDENCE_DIR=$DEVICE_RUN_DIR/evidence
	DEVICE_STDIO_DIR=$DEVICE_EVIDENCE_DIR/stdout-stderr
	DEVICE_METADATA_FILE=$DEVICE_EVIDENCE_DIR/metadata.tsv
	DEVICE_RESULTS_FILE=$DEVICE_EVIDENCE_DIR/results.tsv

	mkdir -m 0700 -- "$DEVICE_WORK_DIR" "$DEVICE_EVIDENCE_DIR" "$DEVICE_STDIO_DIR" || return 2
	printf 'key\tvalue\n' >"$DEVICE_METADATA_FILE"
	printf 'test_id\tstatus\texit_code\tdetail\tstdout\tstderr\n' >"$DEVICE_RESULTS_FILE"
	device_metadata harness_version "$DEVICE_HARNESS_VERSION"
	device_metadata started_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
	device_metadata run_directory "$DEVICE_RUN_DIR"
	device_metadata locale "$LC_ALL"
	device_metadata timezone "$TZ"
	device_metadata umask 0077
}

device_metadata() {
	local key=${1-}
	local value=${2-}
	printf '%s\t%s\n' \
		"$(device_sanitize_tsv "$key")" \
		"$(device_sanitize_tsv "$value")" >>"$DEVICE_METADATA_FILE"
}

device_result() {
	local test_id=${1-}
	local status=${2-}
	local exit_code=${3-}
	local detail=${4-}
	local stdout_path=${5--}
	local stderr_path=${6--}

	if ! device_require_result_id "$test_id"; then
		device_error "invalid result id: $test_id"
		return 2
	fi
	case $status in
		PASS | FAIL | SKIP) ;;
		*)
			device_error "invalid result status for $test_id: $status"
			return 2
			;;
	esac
	if [[ $status == FAIL ]]; then
		((DEVICE_FAILURE_COUNT += 1))
	fi
	printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
		"$test_id" \
		"$status" \
		"$(device_sanitize_tsv "$exit_code")" \
		"$(device_sanitize_tsv "$detail")" \
		"$(device_sanitize_tsv "$stdout_path")" \
		"$(device_sanitize_tsv "$stderr_path")" >>"$DEVICE_RESULTS_FILE"
}

device_capture_paths() {
	local test_id=${1-}
	if ! device_require_result_id "$test_id"; then
		device_error "invalid capture id: $test_id"
		return 2
	fi
	DEVICE_CAPTURE_STDOUT=$DEVICE_STDIO_DIR/$test_id.stdout
	DEVICE_CAPTURE_STDERR=$DEVICE_STDIO_DIR/$test_id.stderr
	: >"$DEVICE_CAPTURE_STDOUT"
	: >"$DEVICE_CAPTURE_STDERR"
}

device_capture() {
	local test_id=$1
	shift
	device_capture_paths "$test_id" || return 2
	"$@" >"$DEVICE_CAPTURE_STDOUT" 2>"$DEVICE_CAPTURE_STDERR"
	DEVICE_CAPTURE_RC=$?
}

device_capture_timed() {
	local seconds=$1
	local test_id=$2
	shift 2
	local child_pid
	local iteration
	local max_iterations=$((seconds * 10))

	device_capture_paths "$test_id" || return 2
	if command -v timeout >/dev/null 2>&1; then
		timeout --signal=KILL "$seconds" "$@" >"$DEVICE_CAPTURE_STDOUT" 2>"$DEVICE_CAPTURE_STDERR"
		DEVICE_CAPTURE_RC=$?
		return 0
	fi

	"$@" >"$DEVICE_CAPTURE_STDOUT" 2>"$DEVICE_CAPTURE_STDERR" &
	child_pid=$!
	for ((iteration = 0; iteration < max_iterations; iteration += 1)); do
		if ! kill -0 "$child_pid" 2>/dev/null; then
			wait "$child_pid"
			DEVICE_CAPTURE_RC=$?
			return 0
		fi
		sleep 0.1
	done
	kill -TERM "$child_pid" 2>/dev/null || true
	sleep 0.1
	kill -KILL "$child_pid" 2>/dev/null || true
	wait "$child_pid" 2>/dev/null || true
	DEVICE_CAPTURE_RC=124
}

device_capture_stdout_rel() {
	printf 'stdout-stderr/%s' "${DEVICE_CAPTURE_STDOUT##*/}"
}

device_capture_stderr_rel() {
	printf 'stdout-stderr/%s' "${DEVICE_CAPTURE_STDERR##*/}"
}

device_wait_for_socket() {
	local pid=$1
	local socket_path=$2
	local iteration
	for ((iteration = 0; iteration < 50; iteration += 1)); do
		if [[ -S $socket_path ]] && kill -0 "$pid" 2>/dev/null; then
			return 0
		fi
		if ! kill -0 "$pid" 2>/dev/null; then
			return 1
		fi
		sleep 0.1
	done
	return 1
}

device_cleanup() {
	local iteration
	local pid=${DEVICE_ACTIVE_DAEMON_PID:-}

	if [[ $pid =~ ^[0-9]+$ ]] && kill -0 "$pid" 2>/dev/null; then
		kill -TERM "$pid" 2>/dev/null || true
		for ((iteration = 0; iteration < 20; iteration += 1)); do
			kill -0 "$pid" 2>/dev/null || break
			sleep 0.1
		done
		if kill -0 "$pid" 2>/dev/null; then
			kill -KILL "$pid" 2>/dev/null || true
		fi
		wait "$pid" 2>/dev/null || true
	fi
	DEVICE_ACTIVE_DAEMON_PID=

	if [[ -n ${DEVICE_RUN_DIR:-} && $DEVICE_RUN_DIR == /* && \
		-n ${DEVICE_WORK_DIR:-} && $DEVICE_WORK_DIR == "$DEVICE_RUN_DIR/work" && \
		-d $DEVICE_WORK_DIR ]]; then
		rm -rf -- "$DEVICE_WORK_DIR"
	fi
}

device_finish() {
	local overall=PASS
	local pass_count
	local fail_count
	local skip_count
	local checksum_file=$DEVICE_EVIDENCE_DIR/SHA256SUMS
	local conclusion_file=$DEVICE_EVIDENCE_DIR/conclusions.md

	if ((DEVICE_FINISHED)); then
		return 0
	fi
	DEVICE_FINISHED=1
	device_metadata finished_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)"

	if command -v sha256sum >/dev/null 2>&1; then
		device_result evidence.sha256 PASS 0 "SHA256SUMS generated" - -
	else
		device_result evidence.sha256 SKIP - "sha256sum is not installed" - -
	fi

	pass_count=$(awk -F '\t' 'NR > 1 && $2 == "PASS" { count += 1 } END { print count + 0 }' "$DEVICE_RESULTS_FILE")
	fail_count=$(awk -F '\t' 'NR > 1 && $2 == "FAIL" { count += 1 } END { print count + 0 }' "$DEVICE_RESULTS_FILE")
	skip_count=$(awk -F '\t' 'NR > 1 && $2 == "SKIP" { count += 1 } END { print count + 0 }' "$DEVICE_RESULTS_FILE")
	if ((fail_count > 0)); then
		overall=FAIL
	fi

	cat >"$conclusion_file" <<EOF
# S0 device conclusions

- Overall automatic status: **$overall**
- PASS: $pass_count
- FAIL: $fail_count
- SKIP: $skip_count
- Evidence directory: \`$DEVICE_EVIDENCE_DIR\`

## Automatic scope

The harness tested only the supplied binary, an isolated synthetic PREFIX,
daemon singleton/stale recovery, and read-only package/service observations.
No package, service, OCI image, or persistent runtime state was mutated.

## Manual review

- Reviewer: _TODO_
- Device/package gate decision: _TODO_
- Accepted limitations: _TODO_
- Follow-up issues: _TODO_
EOF

	if command -v sha256sum >/dev/null 2>&1; then
		(
			cd -- "$DEVICE_EVIDENCE_DIR" || exit 1
			find . -type f ! -name SHA256SUMS -print0 \
				| sort -z \
				| xargs -0 sha256sum
		) >"$checksum_file"
	else
		printf '%s\n' '# sha256sum unavailable; no checksums generated' >"$checksum_file"
	fi

	printf 'S0 evidence: %s\n' "$DEVICE_EVIDENCE_DIR"
}
