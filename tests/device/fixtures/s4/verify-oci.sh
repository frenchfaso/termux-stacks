#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

readonly BASE_DIFF_ID=sha256:b2848c02ac6ff53d265469b5b30f649f335e546a83330cd8916d54e65e640409
# Manifest digest from the reviewed arm64 fixture build. The archive remains
# external and is qualified separately by the operator-supplied SHA-256.
readonly BLESSED_MANIFEST_SHA256=49adfdb8a62394445a96ff32eb48c0f8fce783ec800b1939686b49a74f9ba8ec
readonly EXPECTED_SLOW_ENTRIES=50000
readonly MARKER_VALUE=termux-stacks-s4-v1

usage() {
	printf 'Usage: %s OCI_ARCHIVE EXPECTED_SHA256\n' "${0##*/}"
}

fail() {
	printf 'verify-oci-s4: %s\n' "$*" >&2
	exit 1
}

if (($# != 2)); then
	usage >&2
	exit 2
fi

for required_command in tar gzip jq sha256sum awk; do
	command -v "$required_command" >/dev/null 2>&1 || \
		fail "required command is not installed: $required_command"
done

archive=$1
expected_archive_sha=$2
[[ -f $archive ]] || fail "archive is not a regular file: $archive"
case $archive in
	-*) archive=./$archive ;;
esac

normalize_sha256() {
	local value=$1 label=$2
	value=${value#sha256:}
	[[ ${#value} -eq 64 ]] || fail "$label must contain 64 lowercase hexadecimal characters"
	case $value in
		*[!0-9a-f]*) fail "$label must contain 64 lowercase hexadecimal characters" ;;
	esac
	printf '%s\n' "$value"
}

expected_archive_sha=$(normalize_sha256 "$expected_archive_sha" "expected archive SHA-256")
blessed_manifest_sha=$(normalize_sha256 "$BLESSED_MANIFEST_SHA256" \
	"blessed S4 manifest SHA-256")
blessed_manifest_digest=sha256:$blessed_manifest_sha
archive_sha_line=$(sha256sum -- "$archive") || fail "cannot hash archive"
archive_sha=${archive_sha_line%% *}
[[ $archive_sha == "$expected_archive_sha" ]] || \
	fail "archive SHA-256 mismatch: expected $expected_archive_sha, got $archive_sha"

archive_listing=$(tar -tf "$archive") || fail "cannot list archive"

require_single_entry() {
	local target=$1 entry count=0
	while IFS= read -r entry; do
		[[ $entry == "$target" ]] && count=$((count + 1))
	done <<<"$archive_listing"
	[[ $count -eq 1 ]] || \
		fail "archive must contain exactly one entry named $target (found $count)"
}

read_entry() {
	local target=$1
	tar -xOf "$archive" "$target" || fail "cannot read archive entry: $target"
}

hash_entry() {
	local target=$1 hash_line
	hash_line=$(read_entry "$target" | sha256sum) || fail "cannot hash archive entry: $target"
	printf '%s\n' "${hash_line%% *}"
}

require_sha256_digest() {
	local digest=$1 label=$2 hex
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
	local digest=$1 label=$2 hex path actual
	require_sha256_digest "$digest" "$label"
	hex=${digest#sha256:}
	path=blobs/sha256/$hex
	require_single_entry "$path"
	actual=$(hash_entry "$path")
	[[ $actual == "$hex" ]] || fail "$label hash mismatch: expected $hex, got $actual"
	printf '%s\n' "$path"
}

layer_stream() {
	local path=$1 media_type=$2
	case $media_type in
		application/vnd.oci.image.layer.v1.tar+gzip)
			read_entry "$path" | gzip -dc
			;;
		application/vnd.oci.image.layer.v1.tar)
			read_entry "$path"
			;;
		*) fail "unsupported OCI layer media type: $media_type" ;;
	esac
}

require_single_entry oci-layout
require_single_entry index.json

oci_layout=$(read_entry oci-layout) || fail "cannot load oci-layout"
printf '%s\n' "$oci_layout" | jq -e '.imageLayoutVersion == "1.0.0"' >/dev/null || \
	fail "oci-layout does not declare imageLayoutVersion 1.0.0"

index_json=$(read_entry index.json) || fail "cannot load index.json"
printf '%s\n' "$index_json" | jq -e '
	.schemaVersion == 2 and
	(.manifests | type == "array" and length == 1) and
	(.manifests[0].digest | type == "string")
' >/dev/null || fail "index.json must contain exactly one manifest descriptor"

manifest_digest=$(printf '%s\n' "$index_json" | jq -er '.manifests[0].digest') || \
	fail "cannot read manifest digest"
[[ $manifest_digest == "$blessed_manifest_digest" ]] || \
	fail "manifest is not the blessed S4 fixture: expected $blessed_manifest_digest, got $manifest_digest"
manifest_path=$(verify_blob "$manifest_digest" manifest) || fail "manifest verification failed"
manifest_json=$(read_entry "$manifest_path") || fail "cannot load manifest blob"
printf '%s\n' "$manifest_json" | jq -e '
	.schemaVersion == 2 and
	(.config.digest | type == "string") and
	(.layers | type == "array" and length == 2) and
	(all(.layers[]; (.digest | type == "string") and (.mediaType | type == "string")))
' >/dev/null || fail "manifest must contain one config and exactly two layers"

config_digest=$(printf '%s\n' "$manifest_json" | jq -er '.config.digest') || \
	fail "cannot read config digest"
config_path=$(verify_blob "$config_digest" config) || fail "config verification failed"
config_json=$(read_entry "$config_path") || fail "cannot load config blob"
printf '%s\n' "$config_json" | jq -e --arg base_diff_id "$BASE_DIFF_ID" '
	.os == "linux" and
	.architecture == "arm64" and
	(.config | type == "object") and
	.config.Entrypoint == ["/bin/true"] and
	(((.config | has("Cmd")) == false) or .config.Cmd == null or .config.Cmd == []) and
	.rootfs.type == "layers" and
	(.rootfs.diff_ids | type == "array" and length == 2) and
	.rootfs.diff_ids[0] == $base_diff_id
' >/dev/null || fail "config platform, base layer, Entrypoint, or Cmd violates the S4 contract"

layer_count=0
while IFS=$'\t' read -r layer_digest layer_media_type; do
	[[ -n $layer_digest && -n $layer_media_type ]] || fail "empty layer descriptor"
	verify_blob "$layer_digest" "layer $((layer_count + 1))" >/dev/null
	layer_count=$((layer_count + 1))
done < <(printf '%s\n' "$manifest_json" | jq -r '.layers[] | [.digest, .mediaType] | @tsv')
[[ $layer_count -eq 2 ]] || fail "manifest must contain exactly two layers"

last_layer_digest=$(printf '%s\n' "$manifest_json" | jq -er '.layers[-1].digest') || \
	fail "cannot read final layer digest"
last_layer_media_type=$(printf '%s\n' "$manifest_json" | jq -er '.layers[-1].mediaType') || \
	fail "cannot read final layer media type"
require_sha256_digest "$last_layer_digest" "final slow layer"
last_layer_path=blobs/sha256/${last_layer_digest#sha256:}

slow_entry_count=$(layer_stream "$last_layer_path" "$last_layer_media_type" | \
	tar -tf - | awk '/^s4-slow\/entry-[0-9][0-9]*$/ { count += 1 } END { print count + 0 }') || \
	fail "cannot count slow-layer entries"
((slow_entry_count == EXPECTED_SLOW_ENTRIES)) || \
	fail "final layer has $slow_entry_count slow entries; expected $EXPECTED_SLOW_ENTRIES"

marker=$(layer_stream "$last_layer_path" "$last_layer_media_type" | tar -xOf - s4-fixture) || \
	fail "cannot read S4 fixture marker"
[[ $marker == "$MARKER_VALUE" ]] || fail "S4 fixture marker mismatch"

final_diff_line=$(layer_stream "$last_layer_path" "$last_layer_media_type" | sha256sum) || \
	fail "cannot hash uncompressed final layer"
final_diff_id=sha256:${final_diff_line%% *}
config_final_diff_id=$(printf '%s\n' "$config_json" | jq -er '.rootfs.diff_ids[-1]') || \
	fail "cannot read final diff ID"
[[ $final_diff_id == "$config_final_diff_id" ]] || \
	fail "final layer diff ID mismatch: expected $config_final_diff_id, got $final_diff_id"

printf 'archive_sha256\t%s\n' "$archive_sha"
printf 'manifest_digest\t%s\n' "$manifest_digest"
printf 'config_digest\t%s\n' "$config_digest"
printf 'layer_count\t%s\n' "$layer_count"
printf 'slow_entry_count\t%s\n' "$slow_entry_count"
printf 'final_diff_id\t%s\n' "$final_diff_id"
printf '%s\n' verified
