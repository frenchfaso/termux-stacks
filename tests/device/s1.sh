#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
FIXTURE_DIR=$SCRIPT_DIR/fixtures/s1

DEVICE_PHASE=S1
DEVICE_RUN_LABEL=termux-stacks-s1
DEVICE_RUNTIME_LABEL=txs-s1
DEVICE_HARNESS_VERSION=2
DEVICE_AUTOMATIC_SCOPE=$'The harness used only public proot-distro commands and disposable, exact-name\nfixtures. It tested OCI Entrypoint/Cmd resolution, argv, cwd, ordinary\nnon-reserved environment keys and exit propagation. Session-registry\nreliability and signal/tree-kill semantics remain S2/S3.'

# shellcheck source=tests/device/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
	cat <<'EOF'
Usage:
  bash tests/device/s1.sh [--output-root ABSOLUTE_DIR]

Options:
  --output-root DIR   Existing writable base directory. Defaults to TMPDIR.
  -h, --help          Show this help.

The harness requires the cached reference alpine:3.24.1. Its presence does
not certify cache completeness or an offline build. The harness creates only
random txs-s1-* image references and containers, then removes those exact
targets. It never runs clear-cache, reset, remove --all, or targets an
existing alias.
EOF
}

output_root=
while (($# > 0)); do
	case $1 in
		--output-root)
			[[ $# -ge 2 ]] || {
				device_error "--output-root requires a value"
				exit 2
			}
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

device_init "$output_root" || exit $?

S1_PRE_DIR=$DEVICE_EVIDENCE_DIR/pre
S1_POST_DIR=$DEVICE_EVIDENCE_DIR/post
S1_EXPECTED_DIR=$DEVICE_EVIDENCE_DIR/expected
S1_BUILD_DIR=$DEVICE_EVIDENCE_DIR/build
S1_INSTALL_DIR=$DEVICE_EVIDENCE_DIR/install
S1_GETCMD_DIR=$DEVICE_EVIDENCE_DIR/get-proot-cmd
S1_INTENT_FILE=$DEVICE_EVIDENCE_DIR/intent.tsv
S1_ARGV_FILE=$DEVICE_EVIDENCE_DIR/command-argv.tsv
S1_FIXTURE_FILE=$DEVICE_EVIDENCE_DIR/fixtures.tsv
S1_CLEANUP_FILE=$DEVICE_EVIDENCE_DIR/cleanup.raw
mkdir -m 0700 -- \
	"$S1_PRE_DIR" "$S1_POST_DIR" "$S1_EXPECTED_DIR" "$S1_BUILD_DIR" \
	"$S1_INSTALL_DIR" "$S1_GETCMD_DIR"
printf 'time_utc\taction\ttarget\n' >"$S1_INTENT_FILE"
printf 'test_id\tindex\targument_hex\n' >"$S1_ARGV_FILE"
printf 'shape\timage_ref\tcontainer_alias\tdockerfile_sha256\tprobe_sha256\n' \
	>"$S1_FIXTURE_FILE"
: >"$S1_CLEANUP_FILE"

run_id=$(printf '%x%04x' "$(date +%s)" "$RANDOM")
shapes=(epcmd cmd ep none)
declare -a image_refs=()
declare -a container_aliases=()
declare -A image_for_shape=()
declare -A alias_for_shape=()
declare -A image_intent=()
declare -A container_intent=()

for shape in "${shapes[@]}"; do
	image_ref=txs-s1-$run_id-$shape:v1
	container_alias=txs-s1-$run_id-$shape
	image_refs+=("$image_ref")
	container_aliases+=("$container_alias")
	image_for_shape[$shape]=$image_ref
	alias_for_shape[$shape]=$container_alias
done

S1_CLEANUP_STATE=disabled
S1_CLEANUP_FAILURES=0
S1_DEFERRED_SIGNAL=0

s1_defer_signal() {
	local exit_code=$1
	if ((S1_DEFERRED_SIGNAL == 0)); then
		S1_DEFERRED_SIGNAL=$exit_code
	fi
}

s1_install_deferred_signal_handlers() {
	trap 's1_defer_signal 129' HUP
	trap 's1_defer_signal 130' INT
	trap 's1_defer_signal 143' TERM
}

hex_text() {
	LC_ALL=C printf '%s' "$1" | od -An -v -tx1 | tr -d ' \n'
}

record_intent() {
	local action=$1
	local target=$2
	printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$action" "$target" \
		>>"$S1_INTENT_FILE"
}

record_command() {
	local test_id=$1
	shift
	local index=0
	local argument
	for argument in "$@"; do
		printf '%s\t%s\t%s\n' "$test_id" "$index" "$(hex_text "$argument")" \
			>>"$S1_ARGV_FILE"
		index=$((index + 1))
	done
}

inventory_containers() {
	timeout --signal=KILL 15 env PD_FORCE_NO_COLORS=true proot-distro list --quiet
}

inventory_images() {
	timeout --signal=KILL 15 env PD_FORCE_NO_COLORS=true \
		proot-distro list --image --quiet
}

has_exact_line() {
	local path=$1
	local value=$2
	grep -Fx -- "$value" "$path" >/dev/null 2>&1
}

image_is_present() {
	local target=$1
	local snapshot=$DEVICE_RUNTIME_DIR/images.current
	inventory_images >"$snapshot" 2>/dev/null && has_exact_line "$snapshot" "$target"
}

container_is_present() {
	local target=$1
	local snapshot=$DEVICE_RUNTIME_DIR/containers.current
	inventory_containers >"$snapshot" 2>/dev/null && has_exact_line "$snapshot" "$target"
}

snapshot_state() {
	local directory=$1
	inventory_containers >"$directory/containers.raw" 2>"$directory/containers.stderr" || return 1
	inventory_images >"$directory/images.raw" 2>"$directory/images.stderr" || return 1
}

cleanup_objects() {
	local target
	local current=$DEVICE_RUNTIME_DIR/cleanup.current
	local cleanup_rc

	[[ $S1_CLEANUP_STATE == pending ]] || return 0
	s1_install_deferred_signal_handlers
	S1_CLEANUP_STATE=running
	printf 'cleanup_started\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$S1_CLEANUP_FILE"

	if ! inventory_containers >"$current" 2>>"$S1_CLEANUP_FILE"; then
		printf 'AMBIGUOUS\tcontainer inventory failed; no container removed\n' >>"$S1_CLEANUP_FILE"
		S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
	else
		for target in "${container_aliases[@]}"; do
			if [[ ${container_intent[$target]:-0} == 1 ]] && has_exact_line "$current" "$target"; then
				printf 'REMOVE_CONTAINER\t%s\n' "$target" >>"$S1_CLEANUP_FILE"
				timeout --signal=KILL 15 env PD_FORCE_NO_COLORS=true \
					proot-distro kill "$target" >>"$S1_CLEANUP_FILE" 2>&1 || true
				timeout --signal=KILL 15 env PD_FORCE_NO_COLORS=true \
					proot-distro remove --quiet "$target" \
					>>"$S1_CLEANUP_FILE" 2>&1
				cleanup_rc=$?
				if ((cleanup_rc != 0)); then
					S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
				fi
			fi
		done
	fi

	if ! inventory_images >"$current" 2>>"$S1_CLEANUP_FILE"; then
		printf 'AMBIGUOUS\timage inventory failed; no image removed\n' >>"$S1_CLEANUP_FILE"
		S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
	else
		for target in "${image_refs[@]}"; do
			if [[ ${image_intent[$target]:-0} == 1 ]] && has_exact_line "$current" "$target"; then
				printf 'REMOVE_IMAGE\t%s\n' "$target" >>"$S1_CLEANUP_FILE"
				timeout --signal=KILL 15 env PD_FORCE_NO_COLORS=true \
					proot-distro remove --image --architecture aarch64 --quiet "$target" \
					>>"$S1_CLEANUP_FILE" 2>&1
				cleanup_rc=$?
				if ((cleanup_rc != 0)); then
					S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
				fi
			fi
		done
	fi

	if ! snapshot_state "$S1_POST_DIR"; then
		printf 'AMBIGUOUS\tpost-cleanup inventory failed\n' >>"$S1_CLEANUP_FILE"
		S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
	fi

	for target in "${container_aliases[@]}"; do
		if has_exact_line "$S1_POST_DIR/containers.raw" "$target"; then
			printf 'LEFT_CONTAINER\t%s\n' "$target" >>"$S1_CLEANUP_FILE"
			S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
		fi
	done
	for target in "${image_refs[@]}"; do
		if has_exact_line "$S1_POST_DIR/images.raw" "$target"; then
			printf 'LEFT_IMAGE\t%s\n' "$target" >>"$S1_CLEANUP_FILE"
			S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
		fi
	done
	if ! cmp -s "$S1_PRE_DIR/containers.raw" "$S1_POST_DIR/containers.raw"; then
		printf 'BASELINE_CHANGED\tcontainers\n' >>"$S1_CLEANUP_FILE"
		S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
	fi
	if ! cmp -s "$S1_PRE_DIR/images.raw" "$S1_POST_DIR/images.raw"; then
		printf 'BASELINE_CHANGED\timages\n' >>"$S1_CLEANUP_FILE"
		S1_CLEANUP_FAILURES=$((S1_CLEANUP_FAILURES + 1))
	fi
	printf 'cleanup_finished\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$S1_CLEANUP_FILE"
	S1_CLEANUP_STATE=done
}

s1_on_exit() {
	local original_rc=$1
	local cleanup_was_pending=0
	s1_install_deferred_signal_handlers
	trap - EXIT
	if [[ $S1_CLEANUP_STATE == pending ]]; then
		cleanup_was_pending=1
	fi
	cleanup_objects
	if ((cleanup_was_pending)) && ((DEVICE_FINISHED == 0)); then
		if ((S1_CLEANUP_FAILURES == 0)); then
			device_result cleanup.objects PASS 0 "all exact-name fixtures removed on exit" - -
		else
			device_result cleanup.objects FAIL 1 "cleanup on exit was incomplete or ambiguous" - -
		fi
	fi
	if ((DEVICE_FINISHED == 0)); then
		device_finish || true
	fi
	device_cleanup
	if ((S1_CLEANUP_FAILURES > 0 && original_rc == 0)); then
		original_rc=1
	fi
	if ((DEVICE_FAILURE_COUNT > 0 && original_rc == 0)); then
		original_rc=1
	fi
	if ((S1_DEFERRED_SIGNAL > 0)); then
		original_rc=$S1_DEFERRED_SIGNAL
	fi
	trap - HUP INT TERM
	exit "$original_rc"
}

trap 's1_on_exit $?' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

write_expected() {
	local output=$1
	local cwd=$2
	local image_env=$3
	local added_env=$4
	shift 4
	local index=1
	local argument
	{
		printf 'schema\t1\n'
		printf 'argc\t%s\n' "$#"
		printf 'argv0_hex\t%s\n' "$(hex_text /probe)"
		for argument in "$@"; do
			printf 'argv%s_hex\t%s\n' "$index" "$(hex_text "$argument")"
			index=$((index + 1))
		done
		printf 'cwd_hex\t%s\n' "$(hex_text "$cwd")"
		printf 'image_env_hex\t%s\n' "$(hex_text "$image_env")"
		printf 'added_env_hex\t%s\n' "$(hex_text "$added_env")"
	} >"$output"
}

run_exact_case() {
	local test_id=$1
	local expected_rc=$2
	local expected_file=$3
	shift 3
	local detail
	record_command "$test_id" "$@"
	device_capture_timed 15 "$test_id" env PD_FORCE_NO_COLORS=true "$@"
	if ((DEVICE_CAPTURE_RC == expected_rc)) && cmp -s "$expected_file" "$DEVICE_CAPTURE_STDOUT" && \
		[[ ! -s $DEVICE_CAPTURE_STDERR ]]; then
		detail="exit $expected_rc and deterministic probe output match"
		device_result "$test_id" PASS "$DEVICE_CAPTURE_RC" "$detail" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		detail="expected exit $expected_rc and exact probe output"
		device_result "$test_id" FAIL "$DEVICE_CAPTURE_RC" "$detail" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi
}

run_error_case() {
	local test_id=$1
	local expected_rc=$2
	local error_fragment=$3
	shift 3
	record_command "$test_id" "$@"
	device_capture_timed 15 "$test_id" env PD_FORCE_NO_COLORS=true "$@"
	if ((DEVICE_CAPTURE_RC == expected_rc)) && [[ ! -s $DEVICE_CAPTURE_STDOUT ]] && \
		grep -F -- "$error_fragment" "$DEVICE_CAPTURE_STDERR" >/dev/null; then
		device_result "$test_id" PASS "$DEVICE_CAPTURE_RC" \
			"expected engine error was reported" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		device_result "$test_id" FAIL "$DEVICE_CAPTURE_RC" \
			"engine error exit, stdout, or diagnostic differed" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi
}

preflight_failed=0
for command_name in proot-distro timeout sha256sum od tr grep cmp dpkg-query; do
	if ! command -v "$command_name" >/dev/null 2>&1; then
		device_result "preflight.$command_name" FAIL 127 "$command_name is required" - -
		preflight_failed=1
	fi
done

for shape in "${shapes[@]}"; do
	dockerfile=$FIXTURE_DIR/Dockerfile.$shape
	if [[ ! -f $dockerfile ]]; then
		device_result "preflight.fixture-$shape" FAIL 2 "fixture is missing" - -
		preflight_failed=1
	fi
done
if [[ ! -f $FIXTURE_DIR/probe || ! -x $FIXTURE_DIR/probe ]]; then
	device_result preflight.probe FAIL 2 "probe is missing or not executable" - -
	preflight_failed=1
fi

if ((preflight_failed)); then
	device_finish
	exit 1
fi

device_metadata run_id "$run_id"
device_metadata fixture_directory "$FIXTURE_DIR"
device_metadata harness_sha256 "$(sha256sum "$SCRIPT_DIR/s1.sh" | awk '{ print $1 }')"
device_metadata shared_lib_sha256 "$(sha256sum "$SCRIPT_DIR/lib.sh" | awk '{ print $1 }')"
device_metadata probe_sha256 "$(sha256sum "$FIXTURE_DIR/probe" | awk '{ print $1 }')"
device_metadata architecture "$(uname -m)"
device_metadata android_release "$(getprop ro.build.version.release 2>/dev/null || printf unavailable)"
device_metadata android_sdk "$(getprop ro.build.version.sdk 2>/dev/null || printf unavailable)"
if [[ $(uname -m) != aarch64 || $(dpkg --print-architecture 2>/dev/null) != aarch64 ]]; then
	device_result preflight.architecture FAIL 1 "S1 currently requires a native aarch64 Termux device" - -
	device_finish
	exit 1
fi
device_result preflight.architecture PASS 0 "native aarch64 device confirmed" - -
device_capture inventory.packages dpkg-query -W \
	'-f=${binary:Package}\t${Version}\t${db:Status-Abbrev}\n' proot-distro proot
if ((DEVICE_CAPTURE_RC == 0)); then
	device_result inventory.packages PASS 0 "engine package versions captured" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result inventory.packages FAIL "$DEVICE_CAPTURE_RC" "cannot capture engine versions" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
fi

if snapshot_state "$S1_PRE_DIR"; then
	device_result preflight.inventory PASS 0 "baseline containers and images captured" - -
else
	device_result preflight.inventory FAIL 1 "public engine inventory failed" - -
	device_finish
	exit 1
fi

if ! has_exact_line "$S1_PRE_DIR/images.raw" alpine:3.24.1; then
	device_result preflight.base-image FAIL 1 \
		"the versioned alpine:3.24.1 cache reference is required" - -
	device_finish
	exit 1
fi
device_result preflight.base-image PASS 0 "cached versioned base reference is visible" - -

collision=0
for target in "${container_aliases[@]}"; do
	if has_exact_line "$S1_PRE_DIR/containers.raw" "$target"; then
		device_result preflight.names FAIL 1 "container alias collision: $target" - -
		collision=1
	fi
done
for target in "${image_refs[@]}"; do
	if has_exact_line "$S1_PRE_DIR/images.raw" "$target"; then
		device_result preflight.names FAIL 1 "image reference collision: $target" - -
		collision=1
	fi
done
if ((collision)); then
	device_finish
	exit 1
fi
device_result preflight.names PASS 0 "all random fixture targets were absent" - -

S1_CLEANUP_STATE=pending

probe_sha=$(sha256sum "$FIXTURE_DIR/probe" | awk '{print $1}')
build_failed=0
for shape in "${shapes[@]}"; do
	image_ref=${image_for_shape[$shape]}
	container_alias=${alias_for_shape[$shape]}
	dockerfile=$FIXTURE_DIR/Dockerfile.$shape
	dockerfile_sha=$(sha256sum "$dockerfile" | awk '{print $1}')
	printf '%s\t%s\t%s\t%s\t%s\n' "$shape" "$image_ref" "$container_alias" \
		"$dockerfile_sha" "$probe_sha" >>"$S1_FIXTURE_FILE"

	record_intent build-image "$image_ref"
	image_intent[$image_ref]=1
	record_command "build.$shape" proot-distro build --file "$dockerfile" \
		--tag "$image_ref" "$FIXTURE_DIR"
	device_capture_timed 60 "build.$shape" env PD_FORCE_NO_COLORS=true \
		proot-distro build --file "$dockerfile" --tag "$image_ref" "$FIXTURE_DIR"
	cp "$DEVICE_CAPTURE_STDOUT" "$S1_BUILD_DIR/$shape.stdout"
	cp "$DEVICE_CAPTURE_STDERR" "$S1_BUILD_DIR/$shape.stderr"
	if ((DEVICE_CAPTURE_RC == 0)) && image_is_present "$image_ref"; then
		device_result "build.$shape" PASS 0 "fixture image built and addressable" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		device_result "build.$shape" FAIL "$DEVICE_CAPTURE_RC" \
			"fixture build failed or image reference is absent" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
		build_failed=1
		break
	fi
done

if ((build_failed == 0)); then
	PD_FORCE_NO_COLORS=true proot-distro list --image >"$S1_BUILD_DIR/images.raw" 2>&1 || true
	for shape in "${shapes[@]}"; do
		image_ref=${image_for_shape[$shape]}
		container_alias=${alias_for_shape[$shape]}
		record_intent install-container "$container_alias"
		container_intent[$container_alias]=1
		record_command "install.$shape" proot-distro install --quiet \
			--architecture aarch64 --name "$container_alias" "$image_ref"
		device_capture_timed 60 "install.$shape" env PD_FORCE_NO_COLORS=true \
			proot-distro install --quiet --architecture aarch64 \
			--name "$container_alias" "$image_ref"
		cp "$DEVICE_CAPTURE_STDOUT" "$S1_INSTALL_DIR/$shape.stdout"
		cp "$DEVICE_CAPTURE_STDERR" "$S1_INSTALL_DIR/$shape.stderr"
		if ((DEVICE_CAPTURE_RC == 0)) && container_is_present "$container_alias"; then
			device_result "install.$shape" PASS 0 "fixture container installed" \
				"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
		else
			device_result "install.$shape" FAIL "$DEVICE_CAPTURE_RC" \
				"container install failed or alias is absent" \
				"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
			build_failed=1
			break
		fi
	done
fi

if ((build_failed)); then
	cleanup_objects
	if ((S1_CLEANUP_FAILURES == 0)); then
		device_result cleanup.objects PASS 0 "all owned targets removed after setup failure" - -
	else
		device_result cleanup.objects FAIL 1 "cleanup after setup failure was ambiguous" - -
	fi
	device_finish
	exit 1
fi

override_args=("O one" "" '$HOME' '*' 'semi;colon' '--guest-flag')
epcmd_alias=${alias_for_shape[epcmd]}
cmd_alias=${alias_for_shape[cmd]}
ep_alias=${alias_for_shape[ep]}
none_alias=${alias_for_shape[none]}

expected=$S1_EXPECTED_DIR/epcmd-default.stdout
write_expected "$expected" /fixture-wd image-default '' "E1" "E two" "C1" "C two"
run_exact_case command.epcmd-default 0 "$expected" \
	proot-distro run --isolated "$epcmd_alias"

run_exact_case command.epcmd-bare-separator 0 "$expected" \
	proot-distro run --isolated "$epcmd_alias" --

expected=$S1_EXPECTED_DIR/epcmd-override.stdout
write_expected "$expected" /fixture-wd image-default '' "E1" "E two" "${override_args[@]}"
run_exact_case command.epcmd-override 0 "$expected" \
	proot-distro run --isolated "$epcmd_alias" -- "${override_args[@]}"

expected=$S1_EXPECTED_DIR/cmd-default.stdout
write_expected "$expected" /fixture-wd image-default '' "C1" "C two"
run_exact_case command.cmd-default 0 "$expected" \
	proot-distro run --isolated "$cmd_alias"

expected=$S1_EXPECTED_DIR/cmd-override.stdout
write_expected "$expected" /fixture-wd image-default '' "${override_args[@]}"
run_exact_case command.cmd-override 0 "$expected" \
	proot-distro run --isolated "$cmd_alias" -- /probe "${override_args[@]}"

expected=$S1_EXPECTED_DIR/ep-default.stdout
write_expected "$expected" /fixture-wd image-default '' "E1" "E two"
run_exact_case command.ep-default 0 "$expected" \
	proot-distro run --isolated "$ep_alias"

expected=$S1_EXPECTED_DIR/ep-override.stdout
write_expected "$expected" /fixture-wd image-default '' "E1" "E two" "${override_args[@]}"
run_exact_case command.ep-override 0 "$expected" \
	proot-distro run --isolated "$ep_alias" -- "${override_args[@]}"

run_error_case command.none-default 1 'defines neither Entrypoint nor Cmd' \
	proot-distro run --isolated "$none_alias"

run_error_case command.none-bare-separator 1 'defines neither Entrypoint nor Cmd' \
	proot-distro run --isolated "$none_alias" --

expected=$S1_EXPECTED_DIR/none-override.stdout
write_expected "$expected" /fixture-wd image-default '' "${override_args[@]}"
run_exact_case command.none-override 0 "$expected" \
	proot-distro run --isolated "$none_alias" -- /probe "${override_args[@]}"

expected=$S1_EXPECTED_DIR/env.stdout
write_expected "$expected" /fixture-wd cli added "E1" "E two" "C1" "C two"
run_exact_case command.env 0 "$expected" \
	proot-distro run --isolated --env TSTACK_S1_IMAGE=cli \
	--env TSTACK_S1_ADDED=added "$epcmd_alias"

expected=$S1_EXPECTED_DIR/work-dir.stdout
write_expected "$expected" / image-default '' "E1" "E two" "C1" "C two"
run_exact_case command.work-dir 0 "$expected" \
	proot-distro run --isolated --work-dir / "$epcmd_alias"

expected=$S1_EXPECTED_DIR/exit.stdout
write_expected "$expected" /fixture-wd image-default '' "E1" "E two" "C1" "C two"
run_exact_case command.exit 23 "$expected" \
	proot-distro run --isolated --env TSTACK_S1_EXIT=23 "$epcmd_alias"

record_command command.login proot-distro login --isolated "$epcmd_alias" -- \
	/probe "L one" '$HOME' '*'
device_capture_timed 15 command.login env PD_FORCE_NO_COLORS=true \
	proot-distro login --isolated "$epcmd_alias" -- /probe "L one" '$HOME' '*'
login_subset=$S1_EXPECTED_DIR/login-subset.stdout
{
	printf 'schema\t1\n'
	printf 'argc\t3\n'
	printf 'argv0_hex\t%s\n' "$(hex_text /probe)"
	printf 'argv1_hex\t%s\n' "$(hex_text 'L one')"
	printf 'argv2_hex\t%s\n' "$(hex_text '$HOME')"
	printf 'argv3_hex\t%s\n' "$(hex_text '*')"
} >"$login_subset"
login_ok=1
while IFS= read -r expected_line; do
	grep -Fx -- "$expected_line" "$DEVICE_CAPTURE_STDOUT" >/dev/null || login_ok=0
done <"$login_subset"
if ((DEVICE_CAPTURE_RC == 0 && login_ok == 1)) && [[ ! -s $DEVICE_CAPTURE_STDERR ]]; then
	device_result command.login PASS 0 "login preserved the tested argv through its shell" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result command.login FAIL "$DEVICE_CAPTURE_RC" \
		"login command or argv observation differed" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
fi

record_command getcmd.run proot-distro run --isolated --get-proot-cmd \
	"$epcmd_alias" -- "O one"
device_capture getcmd.run env PD_FORCE_NO_COLORS=true \
	proot-distro run --isolated --get-proot-cmd "$epcmd_alias" -- "O one"
cp "$DEVICE_CAPTURE_STDOUT" "$S1_GETCMD_DIR/run.raw"
if ((DEVICE_CAPTURE_RC == 0)) && grep -F -- /probe "$DEVICE_CAPTURE_STDOUT" >/dev/null && \
	! grep -F -- '/bin/sh -c' "$DEVICE_CAPTURE_STDOUT" >/dev/null; then
	device_result getcmd.run PASS 0 "run assembled a direct image command" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result getcmd.run FAIL "$DEVICE_CAPTURE_RC" "run command assembly was unexpected" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
fi

record_command getcmd.login proot-distro login --isolated --get-proot-cmd \
	"$epcmd_alias" -- /probe "L one"
device_capture getcmd.login env PD_FORCE_NO_COLORS=true \
	proot-distro login --isolated --get-proot-cmd "$epcmd_alias" -- /probe "L one"
cp "$DEVICE_CAPTURE_STDOUT" "$S1_GETCMD_DIR/login.raw"
if ((DEVICE_CAPTURE_RC == 0)) && grep -F -- '/bin/sh' "$DEVICE_CAPTURE_STDOUT" >/dev/null && \
	grep -F -- '-c' "$DEVICE_CAPTURE_STDOUT" >/dev/null; then
	device_result getcmd.login PASS 0 "login assembly exposes its shell boundary" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result getcmd.login FAIL "$DEVICE_CAPTURE_RC" "login shell assembly was unexpected" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
fi

cleanup_objects
if ((S1_CLEANUP_FAILURES == 0)); then
	device_result cleanup.objects PASS 0 "all exact-name fixtures removed; baseline preserved" - -
else
	device_result cleanup.objects FAIL 1 "cleanup was incomplete or baseline changed" - -
fi

device_finish || true
if ((S1_DEFERRED_SIGNAL > 0)); then
	exit "$S1_DEFERRED_SIGNAL"
elif ((DEVICE_FAILURE_COUNT > 0)); then
	exit 1
fi
exit 0
