#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)

DEVICE_PHASE=G3
DEVICE_RUN_LABEL=termux-stacks-g3
DEVICE_RUNTIME_LABEL=txs-g3
DEVICE_HARNESS_VERSION=1
DEVICE_AUTOMATIC_SCOPE=$'The harness exercised only the two explicitly supplied local termux-stacks\npackages and the fixed termux-stacksd service. It required an absent package,\nservice and runtime plus an absent or empty state baseline, used one owned\nmarker, and restored that exact baseline. Only after both ordinary-removal and\nreinstall proofs did it purge that exact package to clear Debian conffiles. It\nnever removed unknown state or targeted an unqualified process.'

# shellcheck source=tests/device/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
	cat <<'EOF'
Usage:
  bash tests/device/g3.sh \
    --old-deb ABSOLUTE_OLD_DEB --new-deb ABSOLUTE_NEW_DEB \
    --accept-package-manager-changes [--output-root ABSOLUTE_DIR]

This acceptance harness installs, upgrades, removes, and reinstalls the
termux-stacks package in the device's real Termux PREFIX. It refuses to start
unless the package, service directory, and runtime directory are absent. A
pre-existing state directory is accepted only when it is a real, mode-0700
empty directory, and is retained. The acknowledgement flag is mandatory.
Only a fully successful matrix authorizes the final exact-package purge needed
to restore an initially absent package record.

Both .deb files must target the device architecture, declare the release
runtime dependencies, and contain successively ordered versions of the same
termux-stacks package. Required dependencies must already be configured; the
harness never installs or upgrades another package.
EOF
}

old_deb_argument=
new_deb_argument=
output_root=
accepted_changes=0
while (($# > 0)); do
	case $1 in
		--old-deb)
			[[ $# -ge 2 ]] || { device_error "--old-deb requires a value"; exit 2; }
			old_deb_argument=$2
			shift 2
			;;
		--new-deb)
			[[ $# -ge 2 ]] || { device_error "--new-deb requires a value"; exit 2; }
			new_deb_argument=$2
			shift 2
			;;
		--output-root)
			[[ $# -ge 2 ]] || { device_error "--output-root requires a value"; exit 2; }
			output_root=$2
			shift 2
			;;
		--accept-package-manager-changes)
			accepted_changes=1
			shift
			;;
		-h | --help)
			usage
			exit 0
			;;
		*)
			device_error "unknown or incomplete argument: $1"
			usage >&2
			exit 2
			;;
	esac
done

if [[ -z $old_deb_argument || -z $new_deb_argument || $accepted_changes -ne 1 ]]; then
	usage >&2
	exit 2
fi

G3_APP_FILES_ROOT=
G3_PREFIX=
G3_HOME=
G3_OUTPUT_ROOT=
G3_RUNTIME_ROOT=

g3_is_below_app_files() {
	local path=$1
	[[ -n $G3_APP_FILES_ROOT ]] || return 1
	case $path/ in
		"$G3_APP_FILES_ROOT"/*) return 0 ;;
		*) return 1 ;;
	esac
}

g3_prepare_app_private_roots() {
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
	G3_PREFIX=$(cd -- "$prefix" 2>/dev/null && pwd -P) || return 2
	G3_APP_FILES_ROOT=$(cd -- "$G3_PREFIX/.." 2>/dev/null && pwd -P) || return 2
	G3_HOME=$(cd -- "$home" 2>/dev/null && pwd -P) || return 2
	if [[ ${G3_PREFIX##*/} != usr || ${G3_APP_FILES_ROOT##*/} != files || \
		$G3_HOME != "$G3_APP_FILES_ROOT/home" ]]; then
		device_error "PREFIX and HOME do not resolve to one canonical Termux app-private files tree"
		return 2
	fi

	effective_output=$requested_output
	[[ -n $effective_output ]] || effective_output=${TMPDIR:-}
	[[ $effective_output == /* && -d $effective_output && ! -L $effective_output ]] || {
		device_error "output root must be an absolute real app-private directory"
		return 2
	}
	G3_OUTPUT_ROOT=$(cd -- "$effective_output" 2>/dev/null && pwd -P) || return 2
	runtime_root=${TMPDIR:-$G3_OUTPUT_ROOT}
	[[ $runtime_root == /* && -d $runtime_root && ! -L $runtime_root ]] || {
		device_error "TMPDIR must be an absolute real app-private directory"
		return 2
	}
	G3_RUNTIME_ROOT=$(cd -- "$runtime_root" 2>/dev/null && pwd -P) || return 2
	if ! g3_is_below_app_files "$G3_OUTPUT_ROOT" || \
		! g3_is_below_app_files "$G3_RUNTIME_ROOT"; then
		device_error "output root and TMPDIR must remain below $G3_APP_FILES_ROOT"
		return 2
	fi
}

g3_prepare_app_private_roots "$output_root" || exit $?
device_init "$output_root" || exit $?

G3_PACKAGES_DIR=$DEVICE_EVIDENCE_DIR/packages
G3_SERVICE_DIR=$G3_PREFIX/var/service/termux-stacksd
G3_STATE_DIR=$G3_PREFIX/var/lib/termux-stacks
G3_RUN_DIR=$G3_PREFIX/var/run/termux-stacks
G3_BINARY=$G3_PREFIX/bin/termux-stacks
G3_SOCKET=$G3_RUN_DIR/daemon.sock
G3_MARKER=$G3_STATE_DIR/g3-package-gate.marker
G3_INTENT_FILE=$DEVICE_EVIDENCE_DIR/intent.tsv
G3_MATRIX_FILE=$DEVICE_EVIDENCE_DIR/matrix.tsv
G3_CLEANUP_FILE=$DEVICE_EVIDENCE_DIR/cleanup.tsv
G3_OLD_DEB=$DEVICE_RUNTIME_DIR/old.deb
G3_NEW_DEB=$DEVICE_RUNTIME_DIR/new.deb
G3_OLD_EXTRACT=$DEVICE_RUNTIME_DIR/old-root
G3_NEW_EXTRACT=$DEVICE_RUNTIME_DIR/new-root
G3_OLD_CONTROL=$DEVICE_RUNTIME_DIR/old-control
G3_NEW_CONTROL=$DEVICE_RUNTIME_DIR/new-control
G3_BLESSED_OLD_SOURCE=1e0c34d2a4498c9f5660662f0dc008aefe1921ab
G3_BLESSED_OLD_DEB_SHA=dd09f17ba225700ce1a18a8477efd67117a42963f4f4f7ee757151d663e4f9b8
G3_BLESSED_OLD_ELF_SHA=78620c23c17d1deb97d0ed7030e47dbf75a2a4732f8eb8bfb7fdbf6fe2b7fc37
G3_OLD_VERSION=
G3_NEW_VERSION=
G3_DEVICE_ARCH=
G3_OLD_BINARY_SHA=
G3_NEW_BINARY_SHA=
G3_MARKER_SHA=
G3_MARKER_OWNED=0
G3_STATE_BASELINE=unknown
G3_MUTATION_STARTED=0
G3_FINAL_PURGE_AUTHORIZED=0
G3_LIVE_PID=
G3_LIVE_STARTTIME=
G3_LIVE_BOOT_ID=
G3_LIVE_EXE_ID=
G3_LAST_INSTALLATION_ID=
G3_DISABLED_INSTALLATION_ID=
G3_LIVE_INSTALLATION_ID=
G3_CLEANUP_FAILURES=0
G3_CLEANUP_STATE=pending
G3_PRESERVE_RUNTIME=0
G3_DEFERRED_SIGNAL=0
G3_RUN_ID=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')

mkdir -m 0700 -- "$G3_PACKAGES_DIR"
printf 'time_utc\taction\ttarget\n' >"$G3_INTENT_FILE"
printf 'case\texpectation\tobserved\n' >"$G3_MATRIX_FILE"
printf 'time_utc\taction\tresult\n' >"$G3_CLEANUP_FILE"

g3_defer_signal() {
	local code=$1
	if ((G3_DEFERRED_SIGNAL == 0)); then G3_DEFERRED_SIGNAL=$code; fi
}

g3_install_deferred_signal_handlers() {
	trap 'g3_defer_signal 129' HUP
	trap 'g3_defer_signal 130' INT
	trap 'g3_defer_signal 143' TERM
}

g3_abort_if_signalled() {
	if ((G3_DEFERRED_SIGNAL != 0)); then
		exit "$G3_DEFERRED_SIGNAL"
	fi
}

g3_intent() {
	local action=$1 target=$2
	printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
		"$(device_sanitize_tsv "$action")" "$(device_sanitize_tsv "$target")" \
		>>"$G3_INTENT_FILE" || return 1
	sync -f "$G3_INTENT_FILE"
}

g3_matrix() {
	printf '%s\t%s\t%s\n' \
		"$(device_sanitize_tsv "$1")" \
		"$(device_sanitize_tsv "$2")" \
		"$(device_sanitize_tsv "$3")" >>"$G3_MATRIX_FILE"
}

g3_package_status() {
	dpkg-query -W '-f=${Status}\t${Version}\n' termux-stacks 2>/dev/null
}

g3_package_absent() {
	local output
	output=$(g3_package_status) && return 1
	[[ -z $output ]]
}

g3_package_config_files() {
	local expected_version=$1 output
	output=$(g3_package_status) || return 1
	[[ $output == $'deinstall ok config-files\t'"$expected_version" ]]
}

g3_proc_starttime() {
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

g3_proc_is_daemon() {
	local pid=$1
	local -a argv=()
	[[ -r /proc/$pid/cmdline ]] || return 1
	mapfile -d '' -t argv <"/proc/$pid/cmdline" || return 1
	((${#argv[@]} == 2)) || return 1
	[[ ${argv[0]} == "$G3_BINARY" && ${argv[1]} == daemon ]]
}

g3_live_identity_matches() {
	local current_boot current_start current_exe
	[[ $G3_LIVE_PID =~ ^[1-9][0-9]*$ ]] || return 1
	current_boot=$(< /proc/sys/kernel/random/boot_id) || return 1
	[[ $current_boot == "$G3_LIVE_BOOT_ID" ]] || return 1
	current_start=$(g3_proc_starttime "$G3_LIVE_PID") || return 1
	[[ $current_start == "$G3_LIVE_STARTTIME" ]] || return 1
	current_exe=$(stat -Lc '%d:%i' "/proc/$G3_LIVE_PID/exe" 2>/dev/null) || return 1
	[[ $current_exe == "$G3_LIVE_EXE_ID" ]] || return 1
	g3_proc_is_daemon "$G3_LIVE_PID"
}

g3_service_pid() {
	local output=$1
	env SVDIR="$G3_PREFIX/var/service" sv status termux-stacksd >"$output" 2>"$output.stderr" || :
	sed -n 's/^run: termux-stacksd: (pid \([1-9][0-9]*\)) .*/\1/p' "$output" | head -n 1
}

g3_wait_recorded_process_gone() {
	local pid=$1 starttime=$2 boot_id=$3 iteration current_boot current_start
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		current_boot=$(< /proc/sys/kernel/random/boot_id) || return 1
		[[ $current_boot == "$boot_id" ]] || return 0
		current_start=$(g3_proc_starttime "$pid" 2>/dev/null) || return 0
		[[ $current_start == "$starttime" ]] || return 0
		sleep 0.1
	done
	return 1
}

g3_wait_for_live_service() {
	local label=$1 pid= iteration
	local status_file=$G3_PACKAGES_DIR/$label.service-status
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		pid=$(g3_service_pid "$status_file")
		if [[ $pid =~ ^[1-9][0-9]*$ && -S $G3_SOCKET ]] && \
			g3_proc_is_daemon "$pid"; then
			G3_LIVE_PID=$pid
			G3_LIVE_STARTTIME=$(g3_proc_starttime "$pid") || return 1
			G3_LIVE_BOOT_ID=$(< /proc/sys/kernel/random/boot_id) || return 1
			G3_LIVE_EXE_ID=$(stat -Lc '%d:%i' "/proc/$pid/exe" 2>/dev/null) || return 1
			return 0
		fi
		sleep 0.1
	done
	return 1
}

g3_pids_for_installed_binary() {
	local binary_id proc_exe proc_id
	local found=0
	[[ -f $G3_BINARY && ! -L $G3_BINARY ]] || return 1
	binary_id=$(stat -Lc '%d:%i' "$G3_BINARY") || return 1
	for proc_exe in /proc/[1-9]*/exe; do
		proc_id=$(stat -Lc '%d:%i' "$proc_exe" 2>/dev/null) || continue
		if [[ $proc_id == "$binary_id" ]]; then
			printf '%s\n' "${proc_exe#/proc/}" | cut -d/ -f1
			found=1
		fi
	done
	return "$found"
}

g3_daemon_pids() {
	local proc_cmd pid
	for proc_cmd in /proc/[1-9]*/cmdline; do
		pid=${proc_cmd#/proc/}
		pid=${pid%/cmdline}
		if g3_proc_is_daemon "$pid"; then printf '%s\n' "$pid"; fi
	done
}

g3_assert_marker() {
	[[ $G3_MARKER_OWNED -eq 1 && -f $G3_MARKER && ! -L $G3_MARKER ]] || return 1
	[[ $(sha256sum "$G3_MARKER" | awk '{print $1}') == "$G3_MARKER_SHA" ]]
}

g3_state_matches_initial_baseline() {
	case $G3_STATE_BASELINE in
		absent) [[ ! -e $G3_STATE_DIR && ! -L $G3_STATE_DIR ]] ;;
		empty-directory)
			[[ -d $G3_STATE_DIR && ! -L $G3_STATE_DIR && \
				$(stat -c '%a' "$G3_STATE_DIR" 2>/dev/null) == 700 && \
				-z $(find "$G3_STATE_DIR" -mindepth 1 -print -quit) ]]
			;;
		*) return 1 ;;
	esac
}

g3_create_marker() {
	case $G3_STATE_BASELINE in
		absent)
			[[ ! -e $G3_STATE_DIR && ! -L $G3_STATE_DIR ]] || return 1
			mkdir -m 0700 -- "$G3_STATE_DIR" || return 1
			;;
		empty-directory)
			[[ -d $G3_STATE_DIR && ! -L $G3_STATE_DIR ]] || return 1
			[[ -z $(find "$G3_STATE_DIR" -mindepth 1 -print -quit) ]] || return 1
			;;
		*) return 1 ;;
	esac
	printf 'termux-stacks-g3-owned-state:%s\n' "$G3_RUN_ID" >"$G3_MARKER" || return 1
	chmod 0600 "$G3_MARKER" || return 1
	sync -f "$G3_MARKER" || return 1
	G3_MARKER_SHA=$(sha256sum "$G3_MARKER" | awk '{print $1}') || return 1
	G3_MARKER_OWNED=1
}

g3_capture_state() {
	local label=$1 output=$G3_PACKAGES_DIR/$label.state.tsv path relative kind hash
	printf 'path\ttype\tsha256\n' >"$output"
	[[ -d $G3_STATE_DIR && ! -L $G3_STATE_DIR ]] || return 1
	while IFS= read -r -d '' path; do
		relative=${path#"$G3_STATE_DIR"/}
		if [[ -f $path && ! -L $path ]]; then
			kind=file
			hash=$(sha256sum "$path" | awk '{print $1}') || return 1
		elif [[ -d $path && ! -L $path ]]; then
			kind=directory
			hash=-
		elif [[ -L $path ]]; then
			kind=symlink
			hash=-
		else
			kind=other
			hash=-
		fi
		printf '%s\t%s\t%s\n' "$(device_sanitize_tsv "$relative")" "$kind" "$hash" >>"$output"
	done < <(find "$G3_STATE_DIR" -mindepth 1 -print0 | sort -z)
}

g3_state_unchanged() {
	local before=$G3_PACKAGES_DIR/$1.state.tsv after=$G3_PACKAGES_DIR/$2.state.tsv
	[[ -f $before && -f $after ]] && cmp -s "$before" "$after"
}

g3_assert_sqlite_database() {
	local header
	[[ -f $G3_STATE_DIR/state.db && ! -L $G3_STATE_DIR/state.db ]] || return 1
	header=$(od -An -N16 -tx1 "$G3_STATE_DIR/state.db" | tr -d ' \n') || return 1
	[[ $header == 53514c69746520666f726d6174203300 ]]
}

g3_capture_database() {
	local label=$1 expected_version=$2 expected_id=${3:-}
	local output=$G3_PACKAGES_DIR/$label.database.tsv version installation_id integrity
	g3_assert_sqlite_database || return 1
	version=$(sqlite3 -batch -readonly "$G3_STATE_DIR/state.db" \
		'PRAGMA user_version;') || return 1
	installation_id=$(sqlite3 -batch -readonly "$G3_STATE_DIR/state.db" \
		"SELECT value FROM meta WHERE key = 'installation_id';") || return 1
	integrity=$(sqlite3 -batch -readonly "$G3_STATE_DIR/state.db" \
		'PRAGMA integrity_check;') || return 1
	printf 'key\tvalue\nuser_version\t%s\ninstallation_id\t%s\nintegrity_check\t%s\n' \
		"$(device_sanitize_tsv "$version")" \
		"$(device_sanitize_tsv "$installation_id")" \
		"$(device_sanitize_tsv "$integrity")" >"$output"
	[[ $version == "$expected_version" && $installation_id =~ ^[0-9a-f]{32}$ && \
		$integrity == ok ]] || return 1
	[[ -z $expected_id || $installation_id == "$expected_id" ]] || return 1
	G3_LAST_INSTALLATION_ID=$installation_id
}

g3_reset_owned_database() {
	local label=$1 path unknown=0
	local -a paths=()
	g3_package_config_files "$G3_NEW_VERSION" || return 1
	[[ -z $G3_LIVE_PID ]] || return 1
	[[ -z $(g3_daemon_pids) ]] || return 1
	[[ ! -e $G3_SOCKET && ! -L $G3_SOCKET ]] || return 1
	g3_assert_marker || return 1
	g3_assert_sqlite_database || return 1
	mapfile -d '' -t paths < <(find "$G3_STATE_DIR" -mindepth 1 -print0)
	for path in "${paths[@]}"; do
		case $path in
			"$G3_MARKER" | "$G3_STATE_DIR/state.db" | "$G3_STATE_DIR/state.db-journal" | \
			"$G3_STATE_DIR/state.db-shm" | "$G3_STATE_DIR/state.db-wal")
				[[ -f $path && ! -L $path ]] || unknown=1
				;;
			*) unknown=1 ;;
		esac
	done
	((unknown == 0)) || return 1
	g3_capture_state "$label-before" || return 1
	g3_intent reset-owned-database \
		"state.db allowlist; marker_sha256=$G3_MARKER_SHA" || return 1
	for path in "$G3_STATE_DIR/state.db-journal" "$G3_STATE_DIR/state.db-shm" \
		"$G3_STATE_DIR/state.db-wal" "$G3_STATE_DIR/state.db"; do
		if [[ -e $path || -L $path ]]; then rm -f -- "$path" || return 1; fi
	done
	sync -f "$G3_MARKER" || return 1
	g3_assert_marker || return 1
	[[ ! -e $G3_STATE_DIR/state.db && ! -L $G3_STATE_DIR/state.db ]] || return 1
	g3_capture_state "$label-after"
}

g3_required_dependencies_configured() {
	local package status
	for package in libsqlite proot-distro termux-services runit; do
		status=$(dpkg-query -W '-f=${Status}' "$package" 2>/dev/null) || return 1
		[[ $status == 'install ok installed' ]] || return 1
	done
	[[ $(dpkg-query -W '-f=${Version}' proot-distro 2>/dev/null) == 5.6.0 ]]
}

g3_static_package() {
	local label=$1 deb=$2 extract=$3 control=$4 require_removal_hooks=$5
	local package version arch depends installed_size prefix_root binary copyright_link
	local expected_machine expected_interpreter expected_conffiles expected_control
	local expected_data actual_control actual_data needed hook report
	local package_ok=1

	report=$G3_PACKAGES_DIR/$label
	dpkg-deb --info "$deb" >"$report.info" 2>"$report.info.stderr" || return 1
	dpkg-deb --contents "$deb" >"$report.contents" 2>"$report.contents.stderr" || return 1
	dpkg-deb --control "$deb" "$control" >"$report.control.stdout" \
		2>"$report.control.stderr" || return 1
	dpkg-deb --extract "$deb" "$extract" >"$report.extract.stdout" \
		2>"$report.extract.stderr" || return 1
	find "$extract" -mindepth 1 -printf '%P\t%y\t%m\n' | sort >"$report.files.tsv" || return 1
	for hook in preinst postinst prerm postrm conffiles; do
		if [[ -f $control/$hook && ! -L $control/$hook ]]; then
			cp -- "$control/$hook" "$report.$hook"
		fi
	done

	package=$(dpkg-deb --field "$deb" Package) || return 1
	version=$(dpkg-deb --field "$deb" Version) || return 1
	arch=$(dpkg-deb --field "$deb" Architecture) || return 1
	depends=$(dpkg-deb --field "$deb" Depends) || return 1
	installed_size=$(dpkg-deb --field "$deb" Installed-Size) || return 1
	prefix_root=$extract$G3_PREFIX
	binary=$prefix_root/bin/termux-stacks
	copyright_link=$prefix_root/share/doc/termux-stacks/copyright

	[[ $package == termux-stacks ]] || package_ok=0
	[[ $version =~ ^[0-9A-Za-z.+:~_-]+$ ]] || package_ok=0
	[[ $arch == "$G3_DEVICE_ARCH" ]] || package_ok=0
	[[ $installed_size =~ ^[0-9]+$ && $installed_size -lt 51200 ]] || package_ok=0
	[[ $(stat -c '%s' "$deb") -lt 52428800 ]] || package_ok=0
	if [[ $label == old ]]; then
		[[ $depends == 'libsqlite, proot-distro (>= 5.6.0), termux-services' ]] || package_ok=0
	else
		[[ $depends == 'libsqlite, proot-distro (= 5.6.0), termux-services' ]] || package_ok=0
	fi

	[[ -f $binary && -x $binary && ! -L $binary ]] || package_ok=0
	[[ -f $prefix_root/var/service/termux-stacksd/run && \
		-x $prefix_root/var/service/termux-stacksd/run && \
		! -L $prefix_root/var/service/termux-stacksd/run ]] || package_ok=0
	[[ -f $prefix_root/var/service/termux-stacksd/down && \
		! -x $prefix_root/var/service/termux-stacksd/down && \
		! -L $prefix_root/var/service/termux-stacksd/down && \
		! -s $prefix_root/var/service/termux-stacksd/down ]] || package_ok=0
	[[ -f $prefix_root/var/service/termux-stacksd/log/run && \
		-x $prefix_root/var/service/termux-stacksd/log/run && \
		! -L $prefix_root/var/service/termux-stacksd/log/run ]] || package_ok=0
	grep -Fxq 'exec "$PREFIX/bin/termux-stacks" daemon 2>&1' \
		"$prefix_root/var/service/termux-stacksd/run" || package_ok=0
	grep -Fq '/share/termux-services/svlogger' \
		"$prefix_root/var/service/termux-stacksd/log/run" || package_ok=0
	[[ -L $copyright_link && \
		$(readlink "$copyright_link") == ../../LICENSES/Apache-2.0.txt ]] || package_ok=0
	[[ ! -e $prefix_root/bin/termux-stacksd && ! -L $prefix_root/bin/termux-stacksd ]] || package_ok=0
	[[ ! -e $prefix_root/var/lib/termux-stacks && ! -L $prefix_root/var/lib/termux-stacks ]] || package_ok=0
	[[ ! -e $prefix_root/var/run/termux-stacks && ! -L $prefix_root/var/run/termux-stacks ]] || package_ok=0
	for hook in preinst postinst; do
		[[ ! -e $control/$hook && ! -L $control/$hook ]] || package_ok=0
	done
	actual_control=$report.control-files
	expected_control=$DEVICE_RUNTIME_DIR/$label.control-files.expected
	if [[ -n $(find "$control" -mindepth 1 -maxdepth 1 ! -type f -print -quit) ]]; then
		package_ok=0
	fi
	find "$control" -mindepth 1 -maxdepth 1 -type f -printf '%f\n' \
		| sort >"$actual_control"
	if ((require_removal_hooks)); then
		printf '%s\n' conffiles control postrm prerm >"$expected_control"
	else
		printf '%s\n' conffiles control >"$expected_control"
	fi
	cmp -s "$expected_control" "$actual_control" || package_ok=0

	actual_data=$report.data-leaves.tsv
	expected_data=$DEVICE_RUNTIME_DIR/$label.data-leaves.expected
	if [[ -n $(find "$extract" -mindepth 1 ! -type d ! -type f ! -type l -print -quit) ]]; then
		package_ok=0
	fi
	find "$extract" -mindepth 1 \( -type f -o -type l \) \
		-printf '%P\t%y\n' | sort >"$actual_data"
	printf '%s\n' \
		"${G3_PREFIX#/}/bin/termux-stacks"$'\t'f \
		"${G3_PREFIX#/}/share/doc/termux-stacks/copyright"$'\t'l \
		"${G3_PREFIX#/}/var/service/termux-stacksd/down"$'\t'f \
		"${G3_PREFIX#/}/var/service/termux-stacksd/log/run"$'\t'f \
		"${G3_PREFIX#/}/var/service/termux-stacksd/run"$'\t'f \
		>"$expected_data"
	cmp -s "$expected_data" "$actual_data" || package_ok=0

	expected_conffiles=$DEVICE_RUNTIME_DIR/$label.conffiles.expected
	printf '%s\n' \
		"$G3_PREFIX/var/service/termux-stacksd/down" \
		"$G3_PREFIX/var/service/termux-stacksd/log/down" \
		"$G3_PREFIX/var/service/termux-stacksd/log/run" \
		"$G3_PREFIX/var/service/termux-stacksd/run" | sort >"$expected_conffiles"
	if [[ -f $control/conffiles && ! -L $control/conffiles ]]; then
		sed '/^[[:space:]]*$/d' "$control/conffiles" | sort >"$report.conffiles.normalized"
		cmp -s "$expected_conffiles" "$report.conffiles.normalized" || package_ok=0
	else
		package_ok=0
	fi
	if [[ -e $control/prerm || -L $control/prerm ]]; then
		[[ -f $control/prerm && -x $control/prerm && ! -L $control/prerm ]] || package_ok=0
		[[ $(head -n 1 "$control/prerm") == "#!$G3_PREFIX/bin/sh" ]] || package_ok=0
		"$G3_PREFIX/bin/sh" -n "$control/prerm" || package_ok=0
		grep -Fq '[ "${1:-}" = remove ]' "$control/prerm" || package_ok=0
		grep -Fq "$G3_PREFIX/bin/sv-disable" "$control/prerm" || package_ok=0
		grep -Fq "$G3_PREFIX/bin/sv\" down termux-stacksd" "$control/prerm" || package_ok=0
		grep -Fq "$G3_PREFIX/var/service/termux-stacksd" "$control/prerm" || package_ok=0
		if grep -Eq '(^|[[:space:]])rm[[:space:]]' "$control/prerm"; then package_ok=0; fi
		if grep -Eq '/var/(lib|run)/termux-stacks|/var/log/(sv/)?termux-stacksd|/var/lib/proot-distro' \
			"$control/prerm"; then
			package_ok=0
		fi
	fi
	if [[ -e $control/postrm || -L $control/postrm ]]; then
		[[ -f $control/postrm && -x $control/postrm && ! -L $control/postrm ]] || package_ok=0
		[[ $(head -n 1 "$control/postrm") == "#!$G3_PREFIX/bin/sh" ]] || package_ok=0
		"$G3_PREFIX/bin/sh" -n "$control/postrm" || package_ok=0
		grep -Fq '[ "${1:-}" = purge ]' "$control/postrm" || package_ok=0
		grep -Fq "rm -rf -- \"$G3_PREFIX/var/service/termux-stacksd\"" \
			"$control/postrm" || package_ok=0
		[[ $(grep -Ec '(^|[[:space:]])rm[[:space:]]' "$control/postrm") -eq 1 ]] || package_ok=0
		if grep -Eq '/var/(lib|run)/termux-stacks|/var/log/(sv/)?termux-stacksd|/var/lib/proot-distro' \
			"$control/postrm"; then
			package_ok=0
		fi
	fi
	if ((require_removal_hooks)); then
		[[ -f $control/prerm && -x $control/prerm && ! -L $control/prerm ]] || package_ok=0
		[[ -f $control/postrm && -x $control/postrm && ! -L $control/postrm ]] || package_ok=0
	fi

	file "$binary" >"$report.elf-file" 2>"$report.elf-file.stderr" || package_ok=0
	readelf -h "$binary" >"$report.elf-header" 2>"$report.elf-header.stderr" || package_ok=0
	readelf -l "$binary" >"$report.elf-program" 2>"$report.elf-program.stderr" || package_ok=0
	readelf -d "$binary" >"$report.elf-dynamic" 2>"$report.elf-dynamic.stderr" || package_ok=0
	readelf -Ws "$binary" >"$report.elf-symbols" 2>"$report.elf-symbols.stderr" || package_ok=0
	readelf -S "$binary" >"$report.elf-sections" 2>"$report.elf-sections.stderr" || package_ok=0
	strings -a "$binary" >"$report.elf-strings" 2>"$report.elf-strings.stderr" || package_ok=0
	needed=$(sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' "$report.elf-dynamic" | sort -u)
	printf '%s\n' "$needed" >"$report.needed"
	[[ $needed == $'libc.so\nlibdl.so\nlibsqlite3.so' ]] || package_ok=0
	grep -q 'Type:.*DYN' "$report.elf-header" || package_ok=0
	grep -Eq 'Entry point address:[[:space:]]+0x[1-9a-fA-F][0-9a-fA-F]*' \
		"$report.elf-header" || package_ok=0
	grep -Eq '[[:space:]]flock(@[^[:space:]]+)?([[:space:]]+\([0-9]+\))?$' \
		"$report.elf-symbols" || package_ok=0
	grep -Eq '[[:space:]]sqlite3_open_v2(@[^[:space:]]+)?([[:space:]]+\([0-9]+\))?$' \
		"$report.elf-symbols" || package_ok=0
	grep -Eq '(^|, )stripped$' "$report.elf-file" || package_ok=0
	if grep -q 'not stripped' "$report.elf-file"; then package_ok=0; fi
	if grep -Eq '\((RPATH|RUNPATH)\)' "$report.elf-dynamic"; then package_ok=0; fi
	if grep -Eq '\.debug_(info|line|str|abbrev)' "$report.elf-sections"; then package_ok=0; fi
	if grep -Eq '[[:space:]]\.symtab([[:space:]]|$)' "$report.elf-sections"; then package_ok=0; fi
	if grep -Eq 'TERMUX_STACKS_(FAULT_DIR|SQLITE_MAX_PAGES|TEST_IMMEDIATE_RESTART)' \
		"$report.elf-strings"; then
		package_ok=0
	fi

	case $G3_DEVICE_ARCH in
		aarch64)
			expected_machine=AArch64
			expected_interpreter=/system/bin/linker64
			;;
		arm)
			expected_machine=ARM
			expected_interpreter=/system/bin/linker
			;;
		i686)
			expected_machine='Intel 80386'
			expected_interpreter=/system/bin/linker
			;;
		x86_64)
			expected_machine='Advanced Micro Devices X86-64'
			expected_interpreter=/system/bin/linker64
			;;
		*) return 1 ;;
	esac
	grep -Fq "Machine:                           $expected_machine" "$report.elf-header" || package_ok=0
	grep -Fq "Requesting program interpreter: $expected_interpreter]" \
		"$report.elf-program" || package_ok=0

	"$binary" --version >"$report.version" 2>"$report.version.stderr" || package_ok=0
	grep -Eq '^termux-stacks [^[:space:]]+$' "$report.version" || package_ok=0

	device_metadata "${label}_package_version" "$version"
	device_metadata "${label}_package_sha256" "$(sha256sum "$deb" | awk '{print $1}')"
	device_metadata "${label}_package_size" "$(stat -c '%s' "$deb")"
	device_metadata "${label}_installed_size_kib" "$installed_size"
	device_metadata "${label}_depends" "$depends"
	if [[ $label == old ]]; then
		G3_OLD_VERSION=$version
		G3_OLD_BINARY_SHA=$(sha256sum "$binary" | awk '{print $1}')
	else
		G3_NEW_VERSION=$version
		G3_NEW_BINARY_SHA=$(sha256sum "$binary" | awk '{print $1}')
	fi
	((package_ok == 1))
}

g3_assert_installed() {
	local expected_version=$1 expected_sha=$2 status version
	status=$(dpkg-query -W '-f=${Status}' termux-stacks 2>/dev/null) || return 1
	version=$(dpkg-query -W '-f=${Version}' termux-stacks 2>/dev/null) || return 1
	[[ $status == 'install ok installed' && $version == "$expected_version" ]] || return 1
	[[ -f $G3_BINARY && -x $G3_BINARY && ! -L $G3_BINARY ]] || return 1
	[[ $(sha256sum "$G3_BINARY" | awk '{print $1}') == "$expected_sha" ]] || return 1
	[[ -d $G3_SERVICE_DIR && ! -L $G3_SERVICE_DIR ]] || return 1
	[[ -x $G3_SERVICE_DIR/run && -x $G3_SERVICE_DIR/log/run ]] || return 1
}

g3_assert_disabled() {
	local label=$1 status_file=$G3_PACKAGES_DIR/$label.service-status pid pids iteration
	[[ -f $G3_SERVICE_DIR/down && ! -L $G3_SERVICE_DIR/down ]] || return 1
	[[ ! -e $G3_SOCKET && ! -L $G3_SOCKET ]] || return 1
	for ((iteration = 0; iteration < 50; iteration += 1)); do
		pid=$(g3_service_pid "$status_file")
		if [[ -z $pid ]] && grep -q '^down: termux-stacksd:' "$status_file"; then break; fi
		sleep 0.1
	done
	[[ -z $pid ]] || return 1
	grep -q '^down: termux-stacksd:' "$status_file" || return 1
	pids=$(g3_pids_for_installed_binary || :)
	[[ -z $pids ]]
}

g3_assert_removed_config() {
	local label=$1 expected_version=$2
	g3_package_config_files "$expected_version" || return 1
	[[ ! -e $G3_BINARY && ! -L $G3_BINARY ]] || return 1
	[[ -d $G3_SERVICE_DIR && ! -L $G3_SERVICE_DIR ]] || return 1
	[[ -f $G3_SERVICE_DIR/run && -x $G3_SERVICE_DIR/run && ! -L $G3_SERVICE_DIR/run ]] || return 1
	[[ -f $G3_SERVICE_DIR/log/run && -x $G3_SERVICE_DIR/log/run && \
		! -L $G3_SERVICE_DIR/log/run ]] || return 1
	g3_assert_disabled "$label" || return 1
	[[ -z $(g3_daemon_pids) ]]
}

g3_enable_service() {
	local label=$1
	g3_abort_if_signalled
	g3_intent sv-enable termux-stacksd || return 1
	device_capture "$label.sv-enable" env SVDIR="$G3_PREFIX/var/service" \
		"$G3_PREFIX/bin/sv-enable" termux-stacksd
	g3_abort_if_signalled
	((DEVICE_CAPTURE_RC == 0)) || return 1
	g3_wait_for_live_service "$label"
}

g3_disable_service() {
	local label=$1 pid=$G3_LIVE_PID starttime=$G3_LIVE_STARTTIME boot_id=$G3_LIVE_BOOT_ID
	[[ $pid =~ ^[1-9][0-9]*$ ]] || return 1
	g3_live_identity_matches || return 1
	g3_abort_if_signalled
	g3_intent sv-disable termux-stacksd || return 1
	device_capture_timed 20 "$label.sv-disable" env SVDIR="$G3_PREFIX/var/service" \
		"$G3_PREFIX/bin/sv-disable" termux-stacksd
	g3_abort_if_signalled
	((DEVICE_CAPTURE_RC == 0)) || return 1
	g3_wait_recorded_process_gone "$pid" "$starttime" "$boot_id" || return 1
	G3_LIVE_PID=
	g3_assert_disabled "$label"
}

g3_restart_service() {
	local label=$1 old_pid=$G3_LIVE_PID old_start=$G3_LIVE_STARTTIME
	local old_boot=$G3_LIVE_BOOT_ID installed_exe_id
	[[ $old_pid =~ ^[1-9][0-9]*$ ]] || return 1
	g3_live_identity_matches || return 1
	[[ ! -e $G3_SERVICE_DIR/down && ! -L $G3_SERVICE_DIR/down ]] || return 1
	installed_exe_id=$(stat -Lc '%d:%i' "$G3_BINARY") || return 1
	g3_abort_if_signalled
	g3_intent sv-restart termux-stacksd || return 1
	device_capture_timed 20 "$label.sv-restart" env SVDIR="$G3_PREFIX/var/service" \
		sv restart termux-stacksd
	g3_abort_if_signalled
	((DEVICE_CAPTURE_RC == 0)) || return 1
	g3_wait_recorded_process_gone "$old_pid" "$old_start" "$old_boot" || return 1
	G3_LIVE_PID=
	g3_wait_for_live_service "$label" || return 1
	[[ $G3_LIVE_PID != "$old_pid" && $G3_LIVE_EXE_ID == "$installed_exe_id" && \
		! -e $G3_SERVICE_DIR/down && ! -L $G3_SERVICE_DIR/down ]]
}

g3_verify_package_files() {
	local label=$1
	dpkg --verify termux-stacks >"$G3_PACKAGES_DIR/$label.dpkg-verify" \
		2>"$G3_PACKAGES_DIR/$label.dpkg-verify.stderr"
	[[ $? -eq 0 && ! -s $G3_PACKAGES_DIR/$label.dpkg-verify ]]
}

g3_capture_package_database() {
	local label=$1 output=$G3_PACKAGES_DIR/$label.dpkg.tsv
	LC_ALL=C dpkg-query -W \
		'-f=${binary:Package}\t${db:Status-Abbrev}\t${Version}\t${Architecture}\n' \
		2>"$output.stderr" | LC_ALL=C sort >"$output"
}

g3_assert_only_target_package_changed() {
	local before=$G3_PACKAGES_DIR/$1.dpkg.tsv after=$G3_PACKAGES_DIR/$2.dpkg.tsv
	local before_other=$DEVICE_RUNTIME_DIR/dpkg-before-other.tsv
	local after_other=$DEVICE_RUNTIME_DIR/dpkg-after-other.tsv
	[[ -f $before && -f $after ]] || return 1
	awk -F '\t' '$1 != "termux-stacks" && $1 !~ /^termux-stacks:/' \
		"$before" >"$before_other" || return 1
	awk -F '\t' '$1 != "termux-stacks" && $1 !~ /^termux-stacks:/' \
		"$after" >"$after_other" || return 1
	cmp -s "$before_other" "$after_other"
}

g3_validate_apt_plan() {
	local plan=$1 mode=$2
	awk -v mode="$mode" '
		/^(Inst|Conf|Remv|Purg) / {
			action_count += 1
			if ($2 != "termux-stacks") bad = 1
			if (mode == "install" && $1 == "Inst") expected = 1
			if (mode == "install" && ($1 == "Remv" || $1 == "Purg")) bad = 1
			if (mode == "remove" && $1 == "Remv") expected = 1
			if (mode == "remove" && $1 != "Remv") bad = 1
			if (mode == "purge" && $1 == "Purg") expected = 1
			if (mode == "purge" && $1 != "Purg" && $1 != "Remv") bad = 1
		}
		END { exit !(action_count > 0 && expected && !bad) }
	' "$plan"
}

g3_plan_install() {
	local label=$1 deb=$2
	device_capture "$label.apt-simulate" apt-get --simulate --no-install-recommends \
		--no-remove --allow-downgrades install "$deb"
	((DEVICE_CAPTURE_RC == 0)) || return 1
	g3_validate_apt_plan "$DEVICE_CAPTURE_STDOUT" install
}

g3_plan_remove() {
	local label=$1
	device_capture "$label.apt-simulate" apt-get --simulate --no-auto-remove \
		remove termux-stacks
	((DEVICE_CAPTURE_RC == 0)) || return 1
	g3_validate_apt_plan "$DEVICE_CAPTURE_STDOUT" remove
}

g3_install() {
	local label=$1 deb=$2 version=$3 sha=$4 effect_rc
	g3_abort_if_signalled
	g3_plan_install "$label" "$deb" || return 1
	g3_abort_if_signalled
	g3_capture_package_database "$label.before" || return 1
	g3_intent apt-install "termux-stacks@$version binary_sha256=$sha" || return 1
	g3_abort_if_signalled
	G3_MUTATION_STARTED=1
	device_capture "$label.apt" apt-get --assume-yes --no-install-recommends \
		--no-remove --no-download --allow-downgrades install "$deb"
	effect_rc=$DEVICE_CAPTURE_RC
	g3_capture_package_database "$label.after" || return 1
	g3_assert_only_target_package_changed "$label.before" "$label.after" || return 1
	g3_abort_if_signalled
	((effect_rc == 0)) || return 1
	g3_assert_installed "$version" "$sha"
}

g3_remove() {
	local label=$1 version effect_rc
	g3_abort_if_signalled
	version=$(dpkg-query -W '-f=${Version}' termux-stacks 2>/dev/null) || return 1
	g3_plan_remove "$label" || return 1
	g3_abort_if_signalled
	g3_capture_package_database "$label.before" || return 1
	g3_intent apt-remove "termux-stacks@$version" || return 1
	g3_abort_if_signalled
	device_capture "$label.apt" apt-get --assume-yes --no-auto-remove \
		remove termux-stacks
	effect_rc=$DEVICE_CAPTURE_RC
	g3_capture_package_database "$label.after" || return 1
	g3_assert_only_target_package_changed "$label.before" "$label.after" || return 1
	g3_abort_if_signalled
	((effect_rc == 0)) || return 1
	g3_assert_removed_config "$label" "$version"
}

g3_stop_owned_daemon() {
	local iteration
	[[ -n $G3_LIVE_PID ]] || return 0
	if ! g3_proc_starttime "$G3_LIVE_PID" >/dev/null 2>&1; then
		G3_LIVE_PID=
		return 0
	fi
	if ! g3_live_identity_matches; then
		printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
			stop-owned-daemon identity-ambiguous >>"$G3_CLEANUP_FILE"
		return 1
	fi
	g3_intent signal-term-qualified-daemon \
		"boot=$G3_LIVE_BOOT_ID pid=$G3_LIVE_PID start=$G3_LIVE_STARTTIME" || return 1
	kill -TERM "$G3_LIVE_PID" || return 1
	for ((iteration = 0; iteration < 100; iteration += 1)); do
		if ! g3_proc_starttime "$G3_LIVE_PID" >/dev/null 2>&1; then
			G3_LIVE_PID=
			return 0
		fi
		sleep 0.1
	done
	if ! g3_live_identity_matches; then return 1; fi
	g3_intent signal-kill-qualified-daemon \
		"boot=$G3_LIVE_BOOT_ID pid=$G3_LIVE_PID start=$G3_LIVE_STARTTIME" || return 1
	kill -KILL "$G3_LIVE_PID" || return 1
	g3_wait_recorded_process_gone "$G3_LIVE_PID" "$G3_LIVE_STARTTIME" \
		"$G3_LIVE_BOOT_ID" || return 1
	G3_LIVE_PID=
}

g3_cleanup_real_paths() {
	local path unknown=0
	local -a paths=()

	if [[ -e $G3_STATE_DIR || -L $G3_STATE_DIR ]]; then
		if [[ $G3_MARKER_OWNED -eq 0 && $G3_STATE_BASELINE == empty-directory && \
			-d $G3_STATE_DIR && ! -L $G3_STATE_DIR && \
			-z $(find "$G3_STATE_DIR" -mindepth 1 -print -quit) ]]; then
			:
		else
			[[ -d $G3_STATE_DIR && ! -L $G3_STATE_DIR && $G3_MARKER_OWNED -eq 1 ]] || return 1
			g3_assert_marker || return 1
			mapfile -d '' -t paths < <(find "$G3_STATE_DIR" -mindepth 1 -print0)
			for path in "${paths[@]}"; do
				case $path in
					"$G3_MARKER" | "$G3_STATE_DIR/state.db" | "$G3_STATE_DIR/state.db-journal" | \
					"$G3_STATE_DIR/state.db-shm" | "$G3_STATE_DIR/state.db-wal")
						[[ -f $path && ! -L $path ]] || { unknown=1; continue; }
						;;
					*) unknown=1 ;;
				esac
			done
			((unknown == 0)) || return 1
			for path in "$G3_MARKER" "$G3_STATE_DIR/state.db-journal" \
				"$G3_STATE_DIR/state.db-shm" "$G3_STATE_DIR/state.db-wal" \
				"$G3_STATE_DIR/state.db"; do
				if [[ -e $path || -L $path ]]; then rm -f -- "$path" || return 1; fi
			done
			if [[ $G3_STATE_BASELINE == absent ]]; then
				rmdir -- "$G3_STATE_DIR" || return 1
			elif [[ $G3_STATE_BASELINE != empty-directory || \
				-n $(find "$G3_STATE_DIR" -mindepth 1 -print -quit) ]]; then
				return 1
			fi
		fi
	fi

	if [[ -e $G3_RUN_DIR || -L $G3_RUN_DIR ]]; then
		[[ -d $G3_RUN_DIR && ! -L $G3_RUN_DIR ]] || return 1
		mapfile -d '' -t paths < <(find "$G3_RUN_DIR" -mindepth 1 -print0)
		for path in "${paths[@]}"; do
			case $path in
				"$G3_RUN_DIR/daemon.lock") [[ -f $path && ! -L $path ]] || return 1 ;;
				"$G3_SOCKET") [[ -S $path && ! -L $path ]] || return 1 ;;
				*) return 1 ;;
			esac
		done
		if [[ -S $G3_SOCKET && ! -L $G3_SOCKET ]]; then rm -f -- "$G3_SOCKET" || return 1; fi
		if [[ -f $G3_RUN_DIR/daemon.lock && ! -L $G3_RUN_DIR/daemon.lock ]]; then
			rm -f -- "$G3_RUN_DIR/daemon.lock" || return 1
		fi
		rmdir -- "$G3_RUN_DIR" || return 1
	fi

	if [[ $G3_STATE_BASELINE == absent ]]; then
		[[ ! -e $G3_STATE_DIR && ! -L $G3_STATE_DIR ]] || return 1
	elif [[ $G3_STATE_BASELINE == empty-directory ]]; then
		[[ -d $G3_STATE_DIR && ! -L $G3_STATE_DIR && \
			$(stat -c '%a' "$G3_STATE_DIR" 2>/dev/null) == 700 && \
			-z $(find "$G3_STATE_DIR" -mindepth 1 -print -quit) ]] || return 1
	else
		return 1
	fi
	[[ ! -e $G3_RUN_DIR && ! -L $G3_RUN_DIR ]]
}

g3_cleanup_purge_package() {
	local version purge_rc
	version=$(dpkg-query -W '-f=${Version}' termux-stacks 2>/dev/null || printf unknown)
	if apt-get --simulate --no-auto-remove purge termux-stacks \
		>"$G3_PACKAGES_DIR/cleanup-apt-simulate.stdout" \
		2>"$G3_PACKAGES_DIR/cleanup-apt-simulate.stderr" && \
		g3_validate_apt_plan "$G3_PACKAGES_DIR/cleanup-apt-simulate.stdout" purge; then
		g3_capture_package_database cleanup-purge.before || return 1
		g3_intent cleanup-apt-purge "termux-stacks@$version" || return 1
		apt-get --assume-yes --no-auto-remove purge termux-stacks \
			>"$G3_PACKAGES_DIR/cleanup-apt-purge.stdout" \
			2>"$G3_PACKAGES_DIR/cleanup-apt-purge.stderr"
		purge_rc=$?
		g3_capture_package_database cleanup-purge.after || return 1
		g3_assert_only_target_package_changed cleanup-purge.before \
			cleanup-purge.after || return 1
		((purge_rc == 0)) || return 1
		g3_package_absent
	else
		return 1
	fi
}

g3_cleanup() {
	local cleanup_rc=0 paths_safe=1 daemon_pids
	[[ $G3_CLEANUP_STATE == pending ]] || return "$G3_CLEANUP_FAILURES"
	G3_CLEANUP_STATE=running
	if ((G3_MUTATION_STARTED == 0)); then
		printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" no-mutation not-required \
			>>"$G3_CLEANUP_FILE"
		G3_CLEANUP_STATE=done
		return 0
	fi

	if [[ -d $G3_SERVICE_DIR && ! -L $G3_SERVICE_DIR && -x $G3_PREFIX/bin/sv-disable ]]; then
		g3_intent cleanup-sv-disable termux-stacksd || cleanup_rc=1
		env SVDIR="$G3_PREFIX/var/service" "$G3_PREFIX/bin/sv-disable" termux-stacksd \
			>"$G3_PACKAGES_DIR/cleanup-sv-disable.stdout" \
			2>"$G3_PACKAGES_DIR/cleanup-sv-disable.stderr" || cleanup_rc=1
	fi
	if ! g3_stop_owned_daemon; then cleanup_rc=1; paths_safe=0; fi
	if ((G3_FINAL_PURGE_AUTHORIZED == 0)); then
		cleanup_rc=1
		paths_safe=0
		printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
			final-purge not-authorized >>"$G3_CLEANUP_FILE"
	elif ! g3_assert_marker || \
		! g3_capture_database before-final-purge-v3 3 "$G3_LIVE_INSTALLATION_ID" || \
		! g3_capture_state before-final-purge; then
		cleanup_rc=1
		paths_safe=0
	elif ! g3_package_status >/dev/null 2>&1 || ! g3_cleanup_purge_package; then
		cleanup_rc=1
		paths_safe=0
	elif ! g3_package_absent || [[ -e $G3_BINARY || -L $G3_BINARY || \
		-e $G3_SERVICE_DIR || -L $G3_SERVICE_DIR ]] || \
		! g3_assert_marker || \
		! g3_capture_database after-final-purge-v3 3 "$G3_LIVE_INSTALLATION_ID" || \
		! g3_capture_state after-final-purge || \
		! g3_state_unchanged before-final-purge after-final-purge; then
		cleanup_rc=1
		paths_safe=0
	fi
	daemon_pids=$(g3_daemon_pids)
	if [[ -n $daemon_pids ]]; then cleanup_rc=1; paths_safe=0; fi
	if ((paths_safe)); then
		if ! g3_intent cleanup-owned-paths \
			"state_baseline=$G3_STATE_BASELINE marker_sha256=${G3_MARKER_SHA:--}"; then
			cleanup_rc=1
			paths_safe=0
		elif ! g3_cleanup_real_paths; then
			cleanup_rc=1
		fi
	fi
	if ((!paths_safe)); then
		G3_PRESERVE_RUNTIME=1
	fi
	if ((cleanup_rc == 0)); then
		printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" exact-baseline restored \
			>>"$G3_CLEANUP_FILE"
	else
		G3_CLEANUP_FAILURES=$((G3_CLEANUP_FAILURES + 1))
		G3_PRESERVE_RUNTIME=1
		device_metadata preserved_real_state "$G3_STATE_DIR"
		device_metadata preserved_real_runtime "$G3_RUN_DIR"
		printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" exact-baseline ambiguous \
			>>"$G3_CLEANUP_FILE"
	fi
	G3_CLEANUP_STATE=done
	return "$cleanup_rc"
}

g3_on_exit() {
	local original_rc=$? finish_rc=0 final_rc
	g3_install_deferred_signal_handlers
	trap - EXIT
	g3_cleanup || original_rc=1
	if ((G3_CLEANUP_FAILURES > 0)); then
		device_result cleanup.objects FAIL 1 \
			"exact package/service/state baseline was not restored; no unknown path was removed" - -
	else
		if ((G3_MUTATION_STARTED)); then
			device_result cleanup.objects PASS 0 \
				"package, service, owned state, and owned runtime returned to the recorded baseline" - -
		else
			device_result cleanup.objects PASS 0 \
				"preflight performed no package, service, or runtime mutation" - -
		fi
	fi
	if ((G3_PRESERVE_RUNTIME)); then
		device_metadata preserved_private_runtime "$DEVICE_RUNTIME_DIR"
		DEVICE_RUNTIME_DIR=
	fi
	device_finish || finish_rc=1
	device_cleanup
	if ((G3_DEFERRED_SIGNAL != 0)); then final_rc=$G3_DEFERRED_SIGNAL
	elif ((original_rc != 0 || finish_rc != 0 || DEVICE_FAILURE_COUNT > 0)); then final_rc=1
	else final_rc=0
	fi
	exit "$final_rc"
}

trap g3_on_exit EXIT
g3_install_deferred_signal_handlers

preflight_ok=1
for command_name in apt-get awk cmp cp cut date dpkg dpkg-deb dpkg-query file find getprop grep head \
	od readelf readlink realpath rmdir sed sha256sum sort stat strings sv sv-enable sv-disable \
	sqlite3 sync tr uname; do
	command -v "$command_name" >/dev/null 2>&1 || preflight_ok=0
done
if [[ $old_deb_argument != /* || ! -f $old_deb_argument || -L $old_deb_argument ]]; then preflight_ok=0; fi
if [[ $new_deb_argument != /* || ! -f $new_deb_argument || -L $new_deb_argument ]]; then preflight_ok=0; fi
if ((preflight_ok)); then
	old_deb_argument=$(realpath "$old_deb_argument") || preflight_ok=0
	new_deb_argument=$(realpath "$new_deb_argument") || preflight_ok=0
fi
if ((preflight_ok)) && [[ $old_deb_argument == "$new_deb_argument" ]]; then preflight_ok=0; fi
if ((preflight_ok)); then
	G3_DEVICE_ARCH=$(dpkg --print-architecture) || preflight_ok=0
fi
[[ $G3_DEVICE_ARCH == aarch64 ]] || preflight_ok=0
if ((preflight_ok)) && ! g3_required_dependencies_configured; then preflight_ok=0; fi
if ((preflight_ok)); then
	dpkg --audit >"$G3_PACKAGES_DIR/preflight.dpkg-audit" \
		2>"$G3_PACKAGES_DIR/preflight.dpkg-audit.stderr" || preflight_ok=0
	[[ ! -s $G3_PACKAGES_DIR/preflight.dpkg-audit ]] || preflight_ok=0
fi
if ((preflight_ok)) && ! g3_package_absent; then preflight_ok=0; fi
for path in "$G3_BINARY" "$G3_SERVICE_DIR" "$G3_RUN_DIR"; do
	if [[ -e $path || -L $path ]]; then preflight_ok=0; fi
done
if ((preflight_ok)) && [[ -n $(g3_daemon_pids) ]]; then preflight_ok=0; fi
if ((preflight_ok)); then
	if [[ ! -e $G3_STATE_DIR && ! -L $G3_STATE_DIR ]]; then
		G3_STATE_BASELINE=absent
	elif [[ -d $G3_STATE_DIR && ! -L $G3_STATE_DIR && \
		$(stat -c '%a' "$G3_STATE_DIR" 2>/dev/null) == 700 && \
		-z $(find "$G3_STATE_DIR" -mindepth 1 -print -quit) ]]; then
		G3_STATE_BASELINE=empty-directory
	else
		preflight_ok=0
	fi
fi
if ((preflight_ok)); then
	old_input_sha=$(sha256sum "$old_deb_argument" | awk '{print $1}') || preflight_ok=0
	new_input_sha=$(sha256sum "$new_deb_argument" | awk '{print $1}') || preflight_ok=0
	[[ $old_input_sha == "$G3_BLESSED_OLD_DEB_SHA" ]] || preflight_ok=0
	cp -- "$old_deb_argument" "$G3_OLD_DEB" || preflight_ok=0
	cp -- "$new_deb_argument" "$G3_NEW_DEB" || preflight_ok=0
	if [[ $(sha256sum "$G3_OLD_DEB" | awk '{print $1}') != "$old_input_sha" || \
		$(sha256sum "$G3_NEW_DEB" | awk '{print $1}') != "$new_input_sha" ]]; then
		preflight_ok=0
	fi
fi
if ((preflight_ok)); then
	g3_static_package old "$G3_OLD_DEB" "$G3_OLD_EXTRACT" "$G3_OLD_CONTROL" 0 || preflight_ok=0
	g3_static_package new "$G3_NEW_DEB" "$G3_NEW_EXTRACT" "$G3_NEW_CONTROL" 1 || preflight_ok=0
fi
if ((preflight_ok)); then
	dpkg --compare-versions "$G3_OLD_VERSION" lt "$G3_NEW_VERSION" || preflight_ok=0
	[[ $G3_OLD_BINARY_SHA == "$G3_BLESSED_OLD_ELF_SHA" ]] || preflight_ok=0
	[[ $G3_OLD_BINARY_SHA != "$G3_NEW_BINARY_SHA" ]] || preflight_ok=0
fi

device_metadata prefix "$G3_PREFIX"
device_metadata home "$G3_HOME"
device_metadata architecture "$G3_DEVICE_ARCH"
device_metadata uname "$(uname -a 2>/dev/null || printf unavailable)"
device_metadata android_release "$(getprop ro.build.version.release 2>/dev/null || printf unavailable)"
device_metadata android_sdk "$(getprop ro.build.version.sdk 2>/dev/null || printf unavailable)"
device_metadata acknowledgement accept-package-manager-changes
device_metadata state_baseline "$G3_STATE_BASELINE"
device_metadata old_source_commit "$G3_BLESSED_OLD_SOURCE"
device_metadata harness_sha256 "$(sha256sum "$SCRIPT_DIR/g3.sh" | awk '{print $1}')"
device_metadata library_sha256 "$(sha256sum "$SCRIPT_DIR/lib.sh" | awk '{print $1}')"

if ((preflight_ok)); then
	device_result preflight PASS 0 \
		"clean Termux baseline and two ordered, architecture-matched package artifacts qualified" - -
else
	device_result preflight FAIL 1 \
		"preflight failed before any package-manager mutation; inspect package reports and baseline" - -
	exit 1
fi

if g3_install fresh-install "$G3_OLD_DEB" "$G3_OLD_VERSION" "$G3_OLD_BINARY_SHA" && \
	g3_assert_disabled fresh-install && g3_state_matches_initial_baseline && \
	g3_verify_package_files fresh-install; then
	g3_matrix fresh-install "old package installed; service disabled; no daemon state" \
		"installed version $G3_OLD_VERSION; down file present; no process or database"
	device_result matrix.fresh-install PASS 0 "fresh install is disabled and inert" - -
else
	g3_matrix fresh-install "old package installed; service disabled; no daemon state" failed
	device_result matrix.fresh-install FAIL 1 "fresh install, layout, or disabled-state check failed" - -
	exit 1
fi

if g3_create_marker && g3_assert_marker; then
	device_result state.marker PASS 0 "owned durable-state marker created after clean install" - -
else
	device_result state.marker FAIL 1 "could not create the exact owned state marker" - -
	exit 1
fi

if g3_enable_service disabled-seed-old && g3_assert_marker && \
	g3_capture_database disabled-seed-v2 2; then
	G3_DISABLED_INSTALLATION_ID=$G3_LAST_INSTALLATION_ID
	device_metadata disabled_migration_installation_id "$G3_DISABLED_INSTALLATION_ID"
	if g3_disable_service disabled-seed-stop && \
		g3_capture_database disabled-seed-stopped-v2 2 "$G3_DISABLED_INSTALLATION_ID" && \
		g3_assert_marker && g3_capture_state disabled-seed-stopped && \
		g3_verify_package_files disabled-seed-stopped; then
		device_result migration.disabled-seed PASS 0 \
			"old daemon initialized schema 2, then stopped without changing its installation identity" - -
	else
		device_result migration.disabled-seed FAIL 1 \
			"old schema-2 seed daemon did not stop cleanly" - -
		exit 1
	fi
else
	device_result migration.disabled-seed FAIL 1 \
		"old package did not initialize a valid schema-2 database" - -
	exit 1
fi

if g3_install upgrade-disabled "$G3_NEW_DEB" "$G3_NEW_VERSION" "$G3_NEW_BINARY_SHA" && \
	g3_assert_disabled upgrade-disabled && \
	g3_capture_database upgrade-disabled-v2 2 "$G3_DISABLED_INSTALLATION_ID" && \
	g3_assert_marker && g3_capture_state upgrade-disabled && \
	g3_state_unchanged disabled-seed-stopped upgrade-disabled && \
	g3_verify_package_files upgrade-disabled; then
	g3_matrix upgrade-disabled \
		"disabled schema-2 package upgraded without automatic start or migration" \
		"installed version $G3_NEW_VERSION; daemon down; schema and installation ID remain v2"
	device_result matrix.upgrade-disabled PASS 0 \
		"disabled package upgrade performs no daemon effect or maintainer-script migration" - -
else
	g3_matrix upgrade-disabled \
		"disabled schema-2 package upgraded without automatic start or migration" failed
	device_result matrix.upgrade-disabled FAIL 1 \
		"disabled upgrade started a daemon, changed schema/state, or failed package verification" - -
	exit 1
fi

if g3_enable_service disabled-migrate-new && \
	g3_capture_database disabled-migrated-v3 3 "$G3_DISABLED_INSTALLATION_ID" && \
	g3_assert_marker; then
	device_capture disabled-migrated.protocol "$G3_BINARY" status g3-package-probe
	if ((DEVICE_CAPTURE_RC == 0)) && \
		grep -Eq '"observed_state"[[:space:]]*:[[:space:]]*"absent"' \
		"$DEVICE_CAPTURE_STDOUT" && \
		g3_disable_service disabled-migrated-stop && \
		g3_capture_database disabled-migrated-stopped-v3 3 "$G3_DISABLED_INSTALLATION_ID" && \
		g3_capture_state disabled-migrated-stopped; then
		g3_matrix migrate-disabled \
			"explicit new-daemon start migrates schema 2 to 3 transactionally" \
			"schema 3, same installation ID and marker; status succeeds; daemon stopped"
		device_result migration.disabled PASS 0 \
			"explicit start after disabled upgrade completed the v2-to-v3 migration" - -
	else
		device_result migration.disabled FAIL 1 \
			"new CLI probe, stop, or migrated-state verification failed" - -
		exit 1
	fi
else
	device_result migration.disabled FAIL 1 \
		"explicit new-daemon start did not produce a valid schema-3 database" - -
	exit 1
fi

if g3_remove remove-disabled && g3_assert_marker && \
	g3_capture_database remove-disabled-v3 3 "$G3_DISABLED_INSTALLATION_ID" && \
	[[ ! -e $G3_SOCKET && ! -L $G3_SOCKET ]] && \
	g3_capture_state after-disabled-remove && \
	g3_state_unchanged disabled-migrated-stopped after-disabled-remove; then
	g3_matrix remove-disabled \
		"ordinary remove leaves exact disabled conffiles; binary gone; migrated state preserved" \
		"config-files status and service skeleton; no binary/socket; schema-3 ID and marker unchanged"
	device_result matrix.remove-disabled PASS 0 \
		"disabled removal follows Debian conffile semantics and preserves migrated durable state" - -
else
	g3_matrix remove-disabled \
		"ordinary remove leaves exact disabled conffiles; binary gone; migrated state preserved" failed
	device_result matrix.remove-disabled FAIL 1 \
		"disabled removal or migrated-state preservation failed" - -
	exit 1
fi

if g3_reset_owned_database live-seed-reset; then
	device_result state.reset PASS 0 \
		"exact harness-owned database files removed after preservation proof; marker retained" - -
else
	device_result state.reset FAIL 1 \
		"database reset found an unknown path or failed its exact allowlist" - -
	exit 1
fi

if g3_install reinstall-old "$G3_OLD_DEB" "$G3_OLD_VERSION" "$G3_OLD_BINARY_SHA" && \
	g3_assert_disabled reinstall-old && g3_assert_marker && \
	[[ ! -e $G3_STATE_DIR/state.db && ! -L $G3_STATE_DIR/state.db ]] && \
	g3_capture_state reinstall-old && \
	g3_state_unchanged live-seed-reset-after reinstall-old && \
	g3_verify_package_files reinstall-old; then
	g3_matrix reinstall-old "old package reinstalled disabled over the preserved marker" \
		"installed version $G3_OLD_VERSION; marker unchanged; no database or daemon"
	device_result matrix.reinstall-old PASS 0 \
		"reinstall remains disabled and preserves the owned marker" - -
else
	g3_matrix reinstall-old "old package reinstalled disabled over the preserved marker" failed
	device_result matrix.reinstall-old FAIL 1 \
		"old-package reinstall or disabled-state check failed" - -
	exit 1
fi

if g3_enable_service live-seed-old && g3_assert_marker && \
	g3_capture_database live-seed-v2 2; then
	G3_LIVE_INSTALLATION_ID=$G3_LAST_INSTALLATION_ID
	device_metadata live_migration_installation_id "$G3_LIVE_INSTALLATION_ID"
	device_metadata live_old_pid "$G3_LIVE_PID"
	device_metadata live_old_starttime "$G3_LIVE_STARTTIME"
	device_metadata live_old_boot_id "$G3_LIVE_BOOT_ID"
	device_metadata live_old_exe_id "$G3_LIVE_EXE_ID"
	device_result service.enable-live PASS 0 \
		"old live daemon initialized a fresh schema-2 database with a qualified identity" - -
else
	device_result service.enable-live FAIL 1 \
		"could not enable and qualify the old schema-2 daemon" - -
	exit 1
fi

g3_capture_state before-live-upgrade || {
	device_result state.before-live-upgrade FAIL 1 "cannot capture state" - -
	exit 1
}
if g3_install upgrade-live "$G3_NEW_DEB" "$G3_NEW_VERSION" "$G3_NEW_BINARY_SHA"; then
	live_stable=1
	for ((stability_cycle = 0; stability_cycle < 50; stability_cycle += 1)); do
		if ! g3_live_identity_matches; then live_stable=0; break; fi
		sleep 0.1
	done
	status_pid=$(g3_service_pid "$G3_PACKAGES_DIR/upgrade-live.service-status")
	if ((live_stable)) && [[ $status_pid == "$G3_LIVE_PID" && \
		! -e $G3_SERVICE_DIR/down && ! -L $G3_SERVICE_DIR/down ]] && \
		g3_assert_marker && \
		g3_capture_database upgrade-live-still-v2 2 "$G3_LIVE_INSTALLATION_ID"; then
		device_capture upgrade-live.protocol-mismatch "$G3_BINARY" status g3-package-probe
		printf '%s\n' 'termux-stacks status: unsupported protocol version 1; expected 2' \
			>"$G3_PACKAGES_DIR/upgrade-live.protocol-mismatch.expected"
		if ((DEVICE_CAPTURE_RC != 0)) && \
			[[ ! -s $DEVICE_CAPTURE_STDOUT ]] && \
			cmp -s "$G3_PACKAGES_DIR/upgrade-live.protocol-mismatch.expected" \
				"$DEVICE_CAPTURE_STDERR" && \
			g3_live_identity_matches && \
			g3_capture_state after-live-upgrade-before-restart && \
			g3_state_unchanged before-live-upgrade \
				after-live-upgrade-before-restart; then
			:
		else
			live_stable=0
		fi
	else
		live_stable=0
	fi
else
	live_stable=0
fi
if ((live_stable)); then
	g3_matrix upgrade-live \
		"enabled old daemon remains mapped; schema stays v2; new CLI fails closed on protocol mismatch" \
		"same boot/PID/start/executable identity for 5 seconds; service enabled; no migration"
	device_result matrix.upgrade-live PASS 0 \
		"live package upgrade does not stop, restart, or migrate the old daemon" - -
else
	g3_matrix upgrade-live \
		"enabled old daemon remains mapped; schema stays v2; new CLI fails closed on protocol mismatch" failed
	device_result matrix.upgrade-live FAIL 1 \
		"live daemon identity, enablement, schema, or fail-closed protocol behavior changed" - -
	exit 1
fi

pre_restart_pid=$G3_LIVE_PID
if g3_restart_service live-migrate-restart && \
	g3_capture_database live-migrated-v3 3 "$G3_LIVE_INSTALLATION_ID" && \
	g3_assert_marker; then
	device_capture live-migrated.protocol "$G3_BINARY" status g3-package-probe
	if ((DEVICE_CAPTURE_RC == 0)) && \
		grep -Eq '"observed_state"[[:space:]]*:[[:space:]]*"absent"' \
		"$DEVICE_CAPTURE_STDOUT" && g3_capture_state after-live-restart; then
		g3_matrix migrate-live \
			"explicit service restart loads new ELF and migrates schema 2 to 3" \
			"PID changed from $pre_restart_pid to $G3_LIVE_PID; schema 3 and installation ID preserved"
		device_result migration.live PASS 0 \
			"explicit restart completed the live v2-to-v3 migration" - -
	else
		device_result migration.live FAIL 1 \
			"new daemon protocol or migrated-state verification failed" - -
		exit 1
	fi
else
	device_result migration.live FAIL 1 \
		"explicit restart did not load the new ELF and migrate schema 2 to 3" - -
	exit 1
fi

g3_capture_state before-live-remove || {
	device_result state.before-live-remove FAIL 1 "cannot capture state" - -
	exit 1
}
removed_live_pid=$G3_LIVE_PID
removed_live_start=$G3_LIVE_STARTTIME
removed_live_boot=$G3_LIVE_BOOT_ID
if g3_remove remove-live && \
	g3_wait_recorded_process_gone "$removed_live_pid" "$removed_live_start" "$removed_live_boot" && \
	[[ ! -e $G3_SOCKET && ! -L $G3_SOCKET ]] && g3_assert_marker && \
	g3_capture_database remove-live-v3 3 "$G3_LIVE_INSTALLATION_ID" && \
	g3_capture_state after-live-remove && \
	g3_state_unchanged before-live-remove after-live-remove; then
	G3_LIVE_PID=
	g3_matrix remove-live \
		"enabled live remove stops exact service and retains disabled conffiles; migrated state survives" \
		"qualified daemon/binary/socket gone; config-files skeleton, schema-3 ID and marker preserved"
	device_result matrix.remove-live PASS 0 \
		"live removal performs exact process cleanup with Debian conffiles and preserves state" - -
else
	g3_matrix remove-live \
		"enabled live remove stops exact service and retains disabled conffiles; migrated state survives" failed
	device_result matrix.remove-live FAIL 1 \
		"live removal, exact process drain, or migrated-state preservation failed" - -
	exit 1
fi

if g3_install reinstall-new "$G3_NEW_DEB" "$G3_NEW_VERSION" "$G3_NEW_BINARY_SHA" && \
	g3_assert_disabled reinstall-new && g3_assert_marker && \
	g3_capture_database reinstall-new-v3 3 "$G3_LIVE_INSTALLATION_ID" && \
	g3_capture_state reinstall-new && \
	g3_state_unchanged after-live-remove reinstall-new && \
	g3_verify_package_files reinstall-new; then
	g3_matrix reinstall-new "new package reinstalled disabled over preserved migrated state" \
		"installed version $G3_NEW_VERSION; down file present; no daemon; schema-3 ID intact"
	device_result matrix.reinstall-new PASS 0 \
		"post-removal reinstall is disabled and preserves migrated state" - -
	G3_FINAL_PURGE_AUTHORIZED=1
else
	g3_matrix reinstall-new "new package reinstalled disabled over preserved migrated state" failed
	device_result matrix.reinstall-new FAIL 1 \
		"new-package reinstall or disabled migrated-state check failed" - -
	exit 1
fi

exit 0
