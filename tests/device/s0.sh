#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=tests/device/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
	cat <<'EOF'
Usage:
  bash tests/device/s0.sh --binary ABSOLUTE_PATH [--output-root ABSOLUTE_DIR]

Options:
  --binary PATH       Already-built termux-stacks binary to test (required).
  --output-root DIR   Existing writable base directory. Defaults to TMPDIR.
  -h, --help          Show this help.

The harness never installs packages, enables services, or uses sudo.
EOF
}

binary_argument=
output_root=

while (($# > 0)); do
	case $1 in
		--binary)
			[[ $# -ge 2 ]] || {
				device_error "--binary requires a value"
				exit 2
			}
			binary_argument=$2
			shift 2
			;;
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

if [[ -z $binary_argument ]]; then
	device_error "--binary is required"
	usage >&2
	exit 2
fi

device_init "$output_root" || exit $?
trap device_cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

binary_path=$binary_argument
if [[ $binary_path != /* ]]; then
	device_result preflight.binary FAIL 2 "binary path must be absolute" - -
	device_finish
	exit 1
fi
if [[ ! -f $binary_path || ! -x $binary_path ]]; then
	device_result preflight.binary FAIL 2 "binary is not a regular executable file: $binary_path" - -
	device_finish
	exit 1
fi
if command -v readlink >/dev/null 2>&1; then
	binary_path=$(readlink -f -- "$binary_path") || {
		device_result preflight.binary FAIL 2 "cannot resolve binary path" - -
		device_finish
		exit 1
	}
fi

device_result preflight.binary PASS 0 "supplied binary is executable" - -
device_metadata binary "$binary_path"
device_metadata prefix "${PREFIX:-<unset>}"
device_metadata tmpdir "${TMPDIR:-<unset>}"
device_metadata uname "$(uname -a 2>/dev/null || printf unavailable)"
device_metadata architecture "$(uname -m 2>/dev/null || printf unavailable)"
device_metadata shell "${SHELL:-<unset>}"

if [[ ${PREFIX:-} == /* && -d ${PREFIX:-} ]]; then
	device_result environment.prefix PASS 0 "PREFIX is an absolute existing directory" - -
	if command -v stat >/dev/null 2>&1; then
		device_metadata prefix_mode "$(stat -c '%a' "$PREFIX" 2>/dev/null || printf unavailable)"
		device_metadata prefix_filesystem "$(stat -f -c '%T' "$PREFIX" 2>/dev/null || printf unavailable)"
	fi
else
	device_result environment.prefix FAIL 1 "PREFIX is unset, relative, or not a directory" - -
fi

if command -v getprop >/dev/null 2>&1; then
	device_metadata android_release "$(getprop ro.build.version.release 2>/dev/null)"
	device_metadata android_sdk "$(getprop ro.build.version.sdk 2>/dev/null)"
	device_metadata android_manufacturer "$(getprop ro.product.manufacturer 2>/dev/null)"
	device_metadata android_model "$(getprop ro.product.model 2>/dev/null)"
fi

if command -v dpkg-query >/dev/null 2>&1; then
	device_capture inventory.packages dpkg-query -W \
		'-f=${binary:Package}\t${Version}\t${db:Status-Abbrev}\n' \
		termux-tools proot-distro termux-services runit libsqlite rust
	if ((DEVICE_CAPTURE_RC == 0)); then
		device_result inventory.packages PASS "$DEVICE_CAPTURE_RC" "relevant package versions captured" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		# dpkg-query exits non-zero when at least one optional package is absent.
		device_result inventory.packages PASS "$DEVICE_CAPTURE_RC" \
			"package inventory captured; one or more optional packages are absent" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi
else
	device_result inventory.packages SKIP - "dpkg-query is not installed" - -
fi

if command -v sha256sum >/dev/null 2>&1; then
	device_capture binary.sha256 sha256sum "$binary_path"
	if ((DEVICE_CAPTURE_RC == 0)); then
		device_metadata binary_sha256 "$(awk '{ print $1; exit }' "$DEVICE_CAPTURE_STDOUT")"
		device_result binary.sha256 PASS 0 "binary digest captured" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		device_result binary.sha256 FAIL "$DEVICE_CAPTURE_RC" "cannot hash binary" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi
else
	device_result binary.sha256 SKIP - "sha256sum is not installed" - -
fi

device_capture binary.version "$binary_path" --version
if ((DEVICE_CAPTURE_RC == 0)) && grep -Eq '^termux-stacks [^[:space:]]+$' "$DEVICE_CAPTURE_STDOUT"; then
	device_result binary.version PASS 0 "version output has the expected shape" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result binary.version FAIL "$DEVICE_CAPTURE_RC" "version command failed or returned unexpected output" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
fi

device_capture binary.help "$binary_path" --help
if ((DEVICE_CAPTURE_RC == 0)) && grep -q 'termux-stacks daemon' "$DEVICE_CAPTURE_STDOUT"; then
	device_result binary.help PASS 0 "help exposes the daemon subcommand" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
else
	device_result binary.help FAIL "$DEVICE_CAPTURE_RC" "help command failed or omitted daemon" \
		"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
fi

if command -v file >/dev/null 2>&1; then
	device_capture binary.file file "$binary_path"
	if ((DEVICE_CAPTURE_RC == 0)) && grep -q ELF "$DEVICE_CAPTURE_STDOUT"; then
		device_result binary.file PASS 0 "file identifies an ELF binary" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		device_result binary.file FAIL "$DEVICE_CAPTURE_RC" "file did not identify an ELF binary" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi
else
	device_result binary.file SKIP - "file is not installed" - -
fi

if command -v readelf >/dev/null 2>&1; then
	device_capture binary.readelf-header readelf -h "$binary_path"
	if ((DEVICE_CAPTURE_RC == 0)) && grep -q 'ELF Header' "$DEVICE_CAPTURE_STDOUT"; then
		device_result binary.readelf-header PASS 0 "ELF header captured" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		device_result binary.readelf-header FAIL "$DEVICE_CAPTURE_RC" "readelf header inspection failed" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi
	device_capture binary.readelf-dynamic readelf -d "$binary_path"
	if ((DEVICE_CAPTURE_RC == 0)); then
		device_result binary.readelf-dynamic PASS 0 "dynamic dependencies captured" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		device_result binary.readelf-dynamic FAIL "$DEVICE_CAPTURE_RC" "readelf dynamic inspection failed" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi
else
	device_result binary.readelf-header SKIP - "readelf is not installed" - -
	device_result binary.readelf-dynamic SKIP - "readelf is not installed" - -
fi

test_prefix=$DEVICE_WORK_DIR/prefix
mkdir -m 0700 -- "$test_prefix"
device_metadata synthetic_prefix "$test_prefix"
state_dir=$test_prefix/var/lib/termux-stacks
run_dir=$test_prefix/var/run/termux-stacks
lock_path=$run_dir/daemon.lock
socket_path=$run_dir/daemon.sock

daemon_stdout_rel=
daemon_stderr_rel=

start_daemon() {
	local label=$1
	device_capture_paths "$label" || return 1
	daemon_stdout_rel=$(device_capture_stdout_rel)
	daemon_stderr_rel=$(device_capture_stderr_rel)
	env PREFIX="$test_prefix" "$binary_path" daemon \
		>"$DEVICE_CAPTURE_STDOUT" 2>"$DEVICE_CAPTURE_STDERR" &
	DEVICE_ACTIVE_DAEMON_PID=$!
	device_wait_for_socket "$DEVICE_ACTIVE_DAEMON_PID" "$socket_path"
}

stop_daemon() {
	local signal=$1
	local pid=${DEVICE_ACTIVE_DAEMON_PID:-}
	local wait_rc
	if [[ ! $pid =~ ^[0-9]+$ ]] || ! kill -0 "$pid" 2>/dev/null; then
		return 1
	fi
	kill -s "$signal" "$pid" 2>/dev/null || return 1
	wait "$pid" 2>/dev/null
	wait_rc=$?
	DEVICE_ACTIVE_DAEMON_PID=
	# A future signal-aware daemon may exit cleanly; the current scaffold exits
	# with 128 + signal. Both are valid as long as the process is gone.
	[[ $wait_rc -eq 0 || $wait_rc -ge 128 ]]
}

if start_daemon daemon.initial; then
	device_result daemon.start PASS 0 "daemon created its control socket" \
		"$daemon_stdout_rel" "$daemon_stderr_rel"

	path_failures=0
	for path_spec in \
		"$state_dir:700:directory" \
		"$run_dir:700:directory" \
		"$lock_path:600:file" \
		"$socket_path:600:socket"; do
		path=${path_spec%%:*}
		remainder=${path_spec#*:}
		expected_mode=${remainder%%:*}
		expected_type=${remainder#*:}
		actual_mode=$(stat -c '%a' "$path" 2>/dev/null || printf missing)
		case $expected_type in
			directory) [[ -d $path && ! -L $path ]] || ((path_failures += 1)) ;;
			file) [[ -f $path && ! -L $path ]] || ((path_failures += 1)) ;;
			socket) [[ -S $path && ! -L $path ]] || ((path_failures += 1)) ;;
		esac
		[[ $actual_mode == "$expected_mode" ]] || ((path_failures += 1))
	done
	if ((path_failures == 0)); then
		device_result daemon.paths PASS 0 "private directories are 0700; lock/socket are 0600" - -
	else
		device_result daemon.paths FAIL 1 "one or more synthetic PREFIX paths have wrong type or mode" - -
	fi

	device_capture_timed 5 daemon.singleton env PREFIX="$test_prefix" "$binary_path" daemon
	if ((DEVICE_CAPTURE_RC != 0 && DEVICE_CAPTURE_RC != 124)) && \
		grep -q 'another daemon is already running' "$DEVICE_CAPTURE_STDERR"; then
		device_result daemon.singleton PASS "$DEVICE_CAPTURE_RC" "second daemon was rejected" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		device_result daemon.singleton FAIL "$DEVICE_CAPTURE_RC" "second daemon was not rejected promptly" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi

	if stop_daemon TERM && start_daemon daemon.after-term; then
		device_result daemon.term-recovery PASS 0 "lock released and stale socket recovered after TERM" \
			"$daemon_stdout_rel" "$daemon_stderr_rel"
	else
		device_result daemon.term-recovery FAIL 1 "daemon did not recover after TERM" \
			"$daemon_stdout_rel" "$daemon_stderr_rel"
	fi

	if [[ -n ${DEVICE_ACTIVE_DAEMON_PID:-} ]] && stop_daemon KILL && \
		start_daemon daemon.after-kill; then
		device_result daemon.kill-recovery PASS 0 "lock released and stale socket recovered after KILL" \
			"$daemon_stdout_rel" "$daemon_stderr_rel"
	else
		device_result daemon.kill-recovery FAIL 1 "daemon did not recover after KILL" \
			"$daemon_stdout_rel" "$daemon_stderr_rel"
	fi

	if [[ -n ${DEVICE_ACTIVE_DAEMON_PID:-} ]]; then
		if stop_daemon TERM; then
			device_result daemon.final-stop PASS 0 "final child daemon stopped" - -
		else
			device_result daemon.final-stop FAIL 1 "final child daemon did not stop" - -
		fi
	fi
else
	start_rc=$?
	device_result daemon.start FAIL "$start_rc" "daemon did not create a control socket" \
		"$daemon_stdout_rel" "$daemon_stderr_rel"
	device_result daemon.paths SKIP - "daemon start failed" - -
	device_result daemon.singleton SKIP - "daemon start failed" - -
	device_result daemon.term-recovery SKIP - "daemon start failed" - -
	device_result daemon.kill-recovery SKIP - "daemon start failed" - -
fi

package_installed=0
if command -v dpkg-query >/dev/null 2>&1; then
	device_capture package.query dpkg-query -W '-f=${Status}\t${Version}\n' termux-stacks
	if ((DEVICE_CAPTURE_RC == 0)) && grep -q '^install ok installed' "$DEVICE_CAPTURE_STDOUT"; then
		package_installed=1
		device_result package.query PASS 0 "termux-stacks package is installed" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	else
		device_result package.query SKIP "$DEVICE_CAPTURE_RC" "termux-stacks package is not installed" \
			"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
	fi
else
	device_result package.query SKIP - "dpkg-query is not installed" - -
fi

if ((package_installed)); then
	installed_binary=${PREFIX}/bin/termux-stacks
	if [[ -x $installed_binary ]]; then
		device_result package.binary PASS 0 "installed package binary exists" - -
		if command -v sha256sum >/dev/null 2>&1; then
			supplied_hash=$(sha256sum "$binary_path" | awk '{ print $1 }')
			installed_hash=$(sha256sum "$installed_binary" | awk '{ print $1 }')
			if [[ $supplied_hash == "$installed_hash" ]]; then
				device_result package.binary-match PASS 0 "supplied binary matches installed package" - -
			else
				device_result package.binary-match FAIL 1 "supplied binary differs from installed package" - -
			fi
		else
			device_result package.binary-match SKIP - "sha256sum is not installed" - -
		fi
	else
		device_result package.binary FAIL 1 "installed package lacks PREFIX/bin/termux-stacks" - -
		device_result package.binary-match SKIP - "installed binary is absent" - -
	fi

	service_dir=${PREFIX}/var/service/termux-stacksd
	if [[ -d $service_dir && -x $service_dir/run && -x $service_dir/log/run ]]; then
		device_result runit.files PASS 0 "service and logger scripts are installed" - -
	else
		device_result runit.files FAIL 1 "installed package lacks runit service files" - -
	fi
	if [[ -e $service_dir/down ]]; then
		device_result runit.default-disabled PASS 0 "service currently has a down file" - -
	else
		device_result runit.default-disabled SKIP - \
			"service is currently enabled; default install state cannot be inferred read-only" - -
	fi
	if command -v sv >/dev/null 2>&1 && [[ -d $service_dir ]]; then
		device_capture_timed 5 runit.status env SVDIR="${PREFIX}/var/service" sv status termux-stacksd
		if ((DEVICE_CAPTURE_RC == 0 || DEVICE_CAPTURE_RC == 1)); then
			device_result runit.status PASS "$DEVICE_CAPTURE_RC" "read-only sv status captured" \
				"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
		else
			device_result runit.status FAIL "$DEVICE_CAPTURE_RC" "sv status failed or timed out" \
				"$(device_capture_stdout_rel)" "$(device_capture_stderr_rel)"
		fi
	else
		device_result runit.status SKIP - "sv or the service directory is absent" - -
	fi
else
	device_result package.binary SKIP - "termux-stacks package is not installed" - -
	device_result package.binary-match SKIP - "termux-stacks package is not installed" - -
	device_result runit.files SKIP - "termux-stacks package is not installed" - -
	device_result runit.default-disabled SKIP - "termux-stacks package is not installed" - -
	device_result runit.status SKIP - "termux-stacks package is not installed" - -
fi

device_finish
if ((DEVICE_FAILURE_COUNT > 0)); then
	exit 1
fi
exit 0
