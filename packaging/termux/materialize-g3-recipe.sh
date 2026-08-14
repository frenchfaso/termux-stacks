#!/usr/bin/env bash

set -euo pipefail

usage() {
	printf 'Usage: %s SOURCE_ARCHIVE OUTPUT_RECIPE\n' "${0##*/}" >&2
	exit 2
}

[[ $# -eq 2 ]] || usage

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../.." && pwd)
source_archive=$1
output_recipe=$2
fixture=$script_dir/build.sh.fixture

[[ $source_archive = /* && -f $source_archive && ! -L $source_archive ]] || {
	printf 'Source archive must be an absolute, regular, non-symlink file: %s\n' \
		"$source_archive" >&2
	exit 1
}
[[ $output_recipe = /* && ! -e $output_recipe && ! -L $output_recipe ]] || {
	printf 'Output recipe must be a new absolute path: %s\n' \
		"$output_recipe" >&2
	exit 1
}
[[ -f $fixture && ! -L $fixture ]] || {
	printf 'Recipe fixture is missing or is not a regular file: %s\n' "$fixture" >&2
	exit 1
}
command -v sha256sum >/dev/null || {
	printf 'sha256sum is required\n' >&2
	exit 1
}

source_archive=$(realpath -- "$source_archive")
source_dir=$(dirname -- "$source_archive")
output_dir=$(dirname -- "$output_recipe")
output_name=${output_recipe##*/}
[[ -d $output_dir && ! -L $output_dir ]] || {
	printf 'Output directory must be an existing real directory: %s\n' \
		"$output_dir" >&2
	exit 1
}
output_dir=$(realpath -- "$output_dir")
output_recipe=$output_dir/$output_name
termux_root=$(realpath -- "$output_dir/../..")
archive_name=${source_archive##*/}

[[ $output_name == build.sh && \
	$output_dir == "$termux_root/packages/termux-stacks" && \
	! -L $termux_root && ! -L $termux_root/packages && \
	-d $termux_root/sources && ! -L $termux_root/sources && \
	$source_dir == "$termux_root/sources" && \
	$archive_name =~ ^[0-9A-Za-z._+-]+\.tar\.gz$ ]] || {
	printf '%s\n' \
		'Source and output must be direct, real children of one Termux package checkout:' \
		"  source: $termux_root/sources/<safe-name>.tar.gz" \
		"  output: $termux_root/packages/termux-stacks/build.sh" >&2
	exit 1
}

cargo_version=$(
	awk '
		$0 == "[package]" { in_package = 1; next }
		in_package && /^\[/ { exit }
		in_package && /^version = "[^"]+"$/ {
			sub(/^version = "/, "")
			sub(/"$/, "")
			print
			exit
		}
	' "$repo_root/Cargo.toml"
)
[[ $cargo_version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?(\+[0-9A-Za-z][0-9A-Za-z.-]*)?$ ]] || {
	printf 'Cargo package version is missing or is not SemVer: %s\n' \
		"${cargo_version:-<missing>}" >&2
	exit 1
}

debian_version=${cargo_version/-/\~}
source_sha256=$(sha256sum "$source_archive" | awk '{print $1}')
source_url='file://${TERMUX_SCRIPTDIR}/sources/'$archive_name

grep -Fxq "TERMUX_PKG_VERSION=$cargo_version" "$fixture" || {
	printf 'Fixture version does not match Cargo package version %s\n' \
		"$cargo_version" >&2
	exit 1
}
for field in TERMUX_PKG_VERSION TERMUX_PKG_SRCURL TERMUX_PKG_SHA256; do
	count=$(grep -c "^${field}=" "$fixture" || true)
	[[ $count -eq 1 ]] || {
		printf 'Fixture must define %s exactly once; found %s\n' "$field" "$count" >&2
		exit 1
	}
done
grep -Fxq 'TERMUX_PKG_SHA256="<sha256>"' "$fixture" || {
	printf 'Fixture checksum placeholder is missing\n' >&2
	exit 1
}

tmp_recipe=$(mktemp "${output_recipe}.tmp.XXXXXXXX")
trap 'rm -f -- "$tmp_recipe"' EXIT

awk \
	-v version="$debian_version" \
	-v source_url="$source_url" \
	-v source_sha256="$source_sha256" '
	/^TERMUX_PKG_VERSION=/ {
		print "TERMUX_PKG_VERSION=" version
		next
	}
	/^TERMUX_PKG_SRCURL=/ {
		print "TERMUX_PKG_SRCURL=\"" source_url "\""
		next
	}
	/^TERMUX_PKG_SHA256=/ {
		print "TERMUX_PKG_SHA256=" source_sha256
		next
	}
	{ print }
' "$fixture" >"$tmp_recipe"

chmod 0644 "$tmp_recipe"
ln -- "$tmp_recipe" "$output_recipe"
rm -f -- "$tmp_recipe"
trap - EXIT

printf 'cargo_version=%s\n' "$cargo_version"
printf 'debian_version=%s\n' "$debian_version"
printf 'source_sha256=%s\n' "$source_sha256"
printf 'source_url=%s\n' "$source_url"
