#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

# Shared helpers for device harnesses.

if [[ -z ${BASH_VERSION:-} ]]; then
	printf '%s\n' "device harness requires Bash" >&2
	return 2 2>/dev/null || exit 2
fi

export LC_ALL=C
export TZ=UTC
umask 077

DEVICE_PHASE=${DEVICE_PHASE:-S0}
DEVICE_RUN_LABEL=${DEVICE_RUN_LABEL:-termux-stacks-s0}
DEVICE_RUNTIME_LABEL=${DEVICE_RUNTIME_LABEL:-txs-s0}
DEVICE_HARNESS_VERSION=${DEVICE_HARNESS_VERSION:-2}
DEVICE_AUTOMATIC_SCOPE=${DEVICE_AUTOMATIC_SCOPE:-$'The harness tested only the supplied binary, an isolated synthetic PREFIX,\ndaemon singleton/stale recovery, and read-only package/service observations.\nNo package, service, OCI image, or persistent runtime state was mutated.'}
DEVICE_RUN_DIR=
DEVICE_WORK_DIR=
DEVICE_RUNTIME_ROOT=
DEVICE_RUNTIME_DIR=
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
	local runtime_root
	if [[ ! $DEVICE_PHASE =~ ^[A-Za-z][A-Za-z0-9_-]*$ ||
		! $DEVICE_RUN_LABEL =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ||
		! $DEVICE_RUNTIME_LABEL =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]]; then
		device_error "invalid harness phase or directory label"
		return 2
	fi

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
	DEVICE_RUN_DIR=$(mktemp -d "$canonical_root/$DEVICE_RUN_LABEL.XXXXXXXX") || {
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

	runtime_root=${TMPDIR:-$canonical_root}
	if [[ $runtime_root != /* || ! -d $runtime_root || ! -w $runtime_root ]]; then
		device_error "TMPDIR must be an absolute writable directory: $runtime_root"
		return 2
	fi
	DEVICE_RUNTIME_ROOT=$(cd -- "$runtime_root" 2>/dev/null && pwd -P) || {
		device_error "cannot resolve runtime root: $runtime_root"
		return 2
	}
	DEVICE_RUNTIME_DIR=$(mktemp -d "$DEVICE_RUNTIME_ROOT/$DEVICE_RUNTIME_LABEL.XXXXXXXX") || {
		device_error "cannot create short runtime directory under $DEVICE_RUNTIME_ROOT"
		return 2
	}
	chmod 0700 "$DEVICE_RUNTIME_DIR" || return 2

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

	case ${DEVICE_RUNTIME_DIR:-} in
		"${DEVICE_RUNTIME_ROOT:-}/$DEVICE_RUNTIME_LABEL".*)
			if [[ -d $DEVICE_RUNTIME_DIR && ! -L $DEVICE_RUNTIME_DIR ]]; then
				rm -rf -- "$DEVICE_RUNTIME_DIR"
			fi
			;;
	esac
}

device_write_conclusions() {
	local conclusion_file=$DEVICE_EVIDENCE_DIR/conclusions.md
	local conclusion_tmp=$DEVICE_EVIDENCE_DIR/.tstack-finalize.conclusions.$$
	local overall=PASS
	local pass_count
	local fail_count
	local skip_count

	pass_count=$(awk -F '\t' 'NR > 1 && $2 == "PASS" { count += 1 } END { print count + 0 }' "$DEVICE_RESULTS_FILE") || return 1
	fail_count=$(awk -F '\t' 'NR > 1 && $2 == "FAIL" { count += 1 } END { print count + 0 }' "$DEVICE_RESULTS_FILE") || return 1
	skip_count=$(awk -F '\t' 'NR > 1 && $2 == "SKIP" { count += 1 } END { print count + 0 }' "$DEVICE_RESULTS_FILE") || return 1
	if ((fail_count > 0)); then
		overall=FAIL
	fi

	if ! cat >"$conclusion_tmp" <<EOF
# $DEVICE_PHASE device conclusions

- Overall automatic status: **$overall**
- PASS: $pass_count
- FAIL: $fail_count
- SKIP: $skip_count
- Evidence directory: \`$DEVICE_EVIDENCE_DIR\`

## Automatic scope

$DEVICE_AUTOMATIC_SCOPE

## Manual review

- Reviewer: _TODO_
- Device/package gate decision: _TODO_
- Accepted limitations: _TODO_
- Follow-up issues: _TODO_
EOF
	then
		rm -f -- "$conclusion_tmp"
		return 1
	fi
	if ! mv -f -- "$conclusion_tmp" "$conclusion_file"; then
		rm -f -- "$conclusion_tmp"
		return 1
	fi
}

device_mark_bundle_failure() {
	local detail=${1:-evidence bundle finalization failed}
	local results_tmp=$DEVICE_EVIDENCE_DIR/.tstack-finalize.results.$$

	if ! awk -F '\t' -v detail="$(device_sanitize_tsv "$detail")" 'BEGIN { OFS = "\t" }
		$1 == "evidence.sha256" {
			$2 = "FAIL"
			$3 = "1"
			$4 = detail
			found = 1
		}
		{ print }
		END { if (!found) exit 1 }' "$DEVICE_RESULTS_FILE" >"$results_tmp"; then
		rm -f -- "$results_tmp"
		return 1
	fi
	if ! mv -f -- "$results_tmp" "$DEVICE_RESULTS_FILE"; then
		rm -f -- "$results_tmp"
		return 1
	fi
	DEVICE_FAILURE_COUNT=$((DEVICE_FAILURE_COUNT + 1))
}

device_finish() {
	local checksum_file=$DEVICE_EVIDENCE_DIR/SHA256SUMS
	local checksum_tmp=$DEVICE_EVIDENCE_DIR/.tstack-finalize.sha256.$$

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

	if ! device_write_conclusions; then
		device_mark_bundle_failure "conclusions generation failed" || \
			DEVICE_FAILURE_COUNT=$((DEVICE_FAILURE_COUNT + 1))
		device_write_conclusions || true
		device_error "cannot write conclusions atomically"
		return 1
	fi

	if command -v sha256sum >/dev/null 2>&1; then
		if (
			set -o pipefail
			cd -- "$DEVICE_EVIDENCE_DIR" || exit 1
			find . -type f ! -name SHA256SUMS \
				! -name '.tstack-finalize.*' -print0 \
				| sort -z \
				| xargs -0 sha256sum
		) >"$checksum_tmp" && mv -f -- "$checksum_tmp" "$checksum_file"; then
			:
		else
			rm -f -- "$checksum_tmp" "$checksum_file"
			device_mark_bundle_failure "SHA256SUMS generation failed" || \
				DEVICE_FAILURE_COUNT=$((DEVICE_FAILURE_COUNT + 1))
			device_write_conclusions || true
			device_error "cannot generate SHA256SUMS"
			return 1
		fi
	else
		if ! printf '%s\n' '# sha256sum unavailable; no checksums generated' \
			>"$checksum_tmp" || ! mv -f -- "$checksum_tmp" "$checksum_file"; then
			rm -f -- "$checksum_tmp"
			device_mark_bundle_failure "checksum placeholder generation failed" || \
				DEVICE_FAILURE_COUNT=$((DEVICE_FAILURE_COUNT + 1))
			device_write_conclusions || true
			device_error "cannot write checksum placeholder"
			return 1
		fi
	fi

	printf '%s evidence: %s\n' "$DEVICE_PHASE" "$DEVICE_EVIDENCE_DIR"
}
