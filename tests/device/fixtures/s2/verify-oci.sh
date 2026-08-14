#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

usage() {
	printf 'Usage: %s OCI_ARCHIVE [EXPECTED_SHA256 [EXPECTED_WORKER_SHA256]]\n' \
		"${0##*/}"
}

fail() {
	printf 'verify-oci: %s\n' "$*" >&2
	exit 1
}

if (($# < 1 || $# > 3)); then
	usage >&2
	exit 2
fi

for required_command in tar gzip jq sha256sum; do
	command -v "$required_command" >/dev/null 2>&1 || \
		fail "required command is not installed: $required_command"
done

archive=$1
expected_archive_sha=${2-}
expected_worker_sha=${3-}

[[ -f $archive ]] || fail "archive is not a regular file: $archive"
case $archive in
	-*) archive=./$archive ;;
esac

if [[ -n $expected_archive_sha ]]; then
	expected_archive_sha=${expected_archive_sha#sha256:}
	[[ ${#expected_archive_sha} -eq 64 ]] || \
		fail "expected SHA-256 must contain 64 lowercase hexadecimal characters"
	case $expected_archive_sha in
		*[!0-9a-f]*)
			fail "expected SHA-256 must contain 64 lowercase hexadecimal characters"
			;;
	esac
fi

if [[ -n $expected_worker_sha ]]; then
	expected_worker_sha=${expected_worker_sha#sha256:}
	[[ ${#expected_worker_sha} -eq 64 ]] || \
		fail "expected worker SHA-256 must contain 64 lowercase hexadecimal characters"
	case $expected_worker_sha in
		*[!0-9a-f]*)
			fail "expected worker SHA-256 must contain 64 lowercase hexadecimal characters"
			;;
	esac
fi

archive_sha_line=$(sha256sum -- "$archive") || fail "cannot hash archive"
archive_sha=${archive_sha_line%% *}
if [[ -n $expected_archive_sha && $archive_sha != "$expected_archive_sha" ]]; then
	fail "archive SHA-256 mismatch: expected $expected_archive_sha, got $archive_sha"
fi

archive_listing=$(tar -tf "$archive") || fail "cannot list archive"

require_single_entry() {
	local target=$1
	local entry
	local count=0

	while IFS= read -r entry; do
		if [[ $entry == "$target" ]]; then
			count=$((count + 1))
		fi
	done <<<"$archive_listing"

	[[ $count -eq 1 ]] || \
		fail "archive must contain exactly one entry named $target (found $count)"
}

read_entry() {
	local target=$1
	tar -xOf "$archive" "$target" || fail "cannot read archive entry: $target"
}

hash_entry() {
	local target=$1
	local hash_line

	hash_line=$(read_entry "$target" | sha256sum) || \
		fail "cannot hash archive entry: $target"
	printf '%s\n' "${hash_line%% *}"
}

require_sha256_digest() {
	local digest=$1
	local label=$2
	local hex

	case $digest in
		sha256:*) hex=${digest#sha256:} ;;
		*) fail "$label does not use sha256: $digest" ;;
	esac
	[[ ${#hex} -eq 64 ]] || fail "$label is not a 64-character SHA-256 digest"
	case $hex in
		*[!0-9a-f]*) fail "$label is not lowercase hexadecimal: $digest" ;;
	esac
}

verify_blob() {
	local digest=$1
	local label=$2
	local hex
	local path
	local actual

	require_sha256_digest "$digest" "$label"
	hex=${digest#sha256:}
	path=blobs/sha256/$hex
	require_single_entry "$path"
	actual=$(hash_entry "$path")
	[[ $actual == "$hex" ]] || \
		fail "$label hash mismatch: expected $hex, got $actual"
	printf '%s\n' "$path"
}

require_single_entry oci-layout
require_single_entry index.json

oci_layout=$(read_entry oci-layout) || fail "cannot load oci-layout"
printf '%s\n' "$oci_layout" | jq -e \
	'.imageLayoutVersion == "1.0.0"' >/dev/null || \
	fail "oci-layout does not declare imageLayoutVersion 1.0.0"

index_json=$(read_entry index.json) || fail "cannot load index.json"
printf '%s\n' "$index_json" | jq -e '
	.schemaVersion == 2 and
	(.manifests | type == "array" and length == 1) and
	(.manifests[0].digest | type == "string")
' >/dev/null || fail "index.json must contain exactly one manifest descriptor"

manifest_digest=$(printf '%s\n' "$index_json" | jq -er '.manifests[0].digest') || \
	fail "cannot read manifest digest"
[[ $manifest_digest == \
	sha256:a096110fe47f4b119a0b04859ab598b6e6c8a57bca0ab1e9364bd35581dd6c5a ]] || \
	fail "manifest digest is not the blessed S2 fixture: $manifest_digest"
manifest_path=$(verify_blob "$manifest_digest" "manifest") || \
	fail "manifest blob verification failed"
manifest_json=$(read_entry "$manifest_path") || fail "cannot load manifest blob"

printf '%s\n' "$manifest_json" | jq -e '
	.schemaVersion == 2 and
	(.config.digest | type == "string") and
	(.layers | type == "array" and length > 0) and
	(all(.layers[]; .digest | type == "string"))
' >/dev/null || fail "manifest has an invalid config or layer descriptor set"

config_digest=$(printf '%s\n' "$manifest_json" | jq -er '.config.digest') || \
	fail "cannot read config digest"
config_path=$(verify_blob "$config_digest" "config") || \
	fail "config blob verification failed"
config_json=$(read_entry "$config_path") || fail "cannot load config blob"

printf '%s\n' "$config_json" | jq -e '
	.os == "linux" and
	.architecture == "arm64" and
	(.config | type == "object") and
	.config.Entrypoint == ["/s2-worker"] and
	(((.config | has("Cmd")) == false) or .config.Cmd == null or .config.Cmd == []) and
	.rootfs.type == "layers" and
	(.rootfs.diff_ids | type == "array" and length == 2) and
	.rootfs.diff_ids[0] == "sha256:b2848c02ac6ff53d265469b5b30f649f335e546a83330cd8916d54e65e640409"
' >/dev/null || \
	fail "config platform, base rootfs, Entrypoint, or Cmd is not the S2 fixture contract"

layer_digests=$(printf '%s\n' "$manifest_json" | jq -er '.layers[].digest') || \
	fail "cannot read layer digests"
layer_count=0
while IFS= read -r layer_digest; do
	[[ -n $layer_digest ]] || fail "manifest contains an empty layer digest"
	verify_blob "$layer_digest" "layer $((layer_count + 1))" >/dev/null
	layer_count=$((layer_count + 1))
done <<<"$layer_digests"
((layer_count > 0)) || fail "manifest contains no layers"

if [[ -n $expected_worker_sha ]]; then
	last_layer_digest=$(printf '%s\n' "$manifest_json" | jq -er '.layers[-1].digest') || \
		fail "cannot read final worker layer digest"
	last_layer_media_type=$(printf '%s\n' "$manifest_json" | jq -er '.layers[-1].mediaType') || \
		fail "cannot read final worker layer media type"
	require_sha256_digest "$last_layer_digest" "final worker layer"
	last_layer_path=blobs/sha256/${last_layer_digest#sha256:}
	case $last_layer_media_type in
		application/vnd.oci.image.layer.v1.tar+gzip)
			worker_hash_line=$(read_entry "$last_layer_path" | gzip -dc | \
				tar -xOf - s2-worker | sha256sum) || \
				fail "cannot extract and hash s2-worker from final gzip layer"
			;;
		application/vnd.oci.image.layer.v1.tar)
			worker_hash_line=$(read_entry "$last_layer_path" | \
				tar -xOf - s2-worker | sha256sum) || \
				fail "cannot extract and hash s2-worker from final layer"
			;;
		*) fail "unsupported final worker layer media type: $last_layer_media_type" ;;
	esac
	worker_sha=${worker_hash_line%% *}
	[[ $worker_sha == "$expected_worker_sha" ]] || \
		fail "archived s2-worker SHA-256 mismatch: expected $expected_worker_sha, got $worker_sha"
fi

printf 'archive_sha256\t%s\n' "$archive_sha"
printf 'manifest_digest\t%s\n' "$manifest_digest"
printf 'config_digest\t%s\n' "$config_digest"
printf 'layer_count\t%s\n' "$layer_count"
if [[ -n $expected_worker_sha ]]; then
	printf 'worker_sha256\t%s\n' "$worker_sha"
fi
printf '%s\n' 'verified'
