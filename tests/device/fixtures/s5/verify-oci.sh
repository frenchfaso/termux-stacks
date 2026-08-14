#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

readonly BLESSED_MANIFEST=sha256:c8013f399d05336a7870eaf9a70a74a0391f57e473250cb84a46f159cc7e24e6
readonly BLESSED_CONFIG=sha256:1991bd789d7184290c3cce84fd6af068b8b745e9bddf178661ce7f5ecf68135c
readonly BLESSED_LAYER=sha256:e796369152ae2bcfc5a6770ec686c48258300b27adec12edb4c13f9ab41af2f5
readonly BLESSED_DIFF_ID=sha256:b2848c02ac6ff53d265469b5b30f649f335e546a83330cd8916d54e65e640409

fail() {
	printf 'verify-oci: %s\n' "$*" >&2
	exit 1
}

if (($# != 2)); then
	printf 'Usage: %s ABSOLUTE_OCI_ARCHIVE EXPECTED_SHA256\n' "${0##*/}" >&2
	exit 2
fi

for command_name in tar jq sha256sum; do
	command -v "$command_name" >/dev/null 2>&1 || \
		fail "required command is not installed: $command_name"
done

archive=$1
expected_archive_sha=$2
[[ $archive == /* && -f $archive && ! -L $archive ]] || \
	fail "archive must be an absolute regular non-symlink file"
[[ $expected_archive_sha =~ ^[0-9a-f]{64}$ ]] || \
	fail "expected SHA-256 must be 64 lowercase hexadecimal characters"

archive_sha=$(sha256sum -- "$archive") || fail "cannot hash archive"
archive_sha=${archive_sha%% *}
[[ $archive_sha == "$expected_archive_sha" ]] || \
	fail "archive SHA-256 mismatch: expected $expected_archive_sha, got $archive_sha"

listing=$(tar -tf "$archive") || fail "cannot list archive"

require_single_entry() {
	local target=$1 count
	count=$(printf '%s\n' "$listing" | awk -v target="$target" '$0 == target { count += 1 } END { print count + 0 }') || \
		fail "cannot count archive entry: $target"
	[[ $count -eq 1 ]] || fail "archive must contain exactly one $target entry"
}

read_entry() {
	tar -xOf "$archive" "$1" || fail "cannot read archive entry: $1"
}

verify_blob() {
	local digest=$1 label=$2 hex path actual
	[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || fail "$label does not use a valid SHA-256 digest"
	hex=${digest#sha256:}
	path=blobs/sha256/$hex
	require_single_entry "$path"
	actual=$(read_entry "$path" | sha256sum) || fail "cannot hash $label"
	actual=${actual%% *}
	[[ $actual == "$hex" ]] || fail "$label content does not match its digest"
	printf '%s\n' "$path"
}

require_single_entry oci-layout
require_single_entry index.json
read_entry oci-layout | jq -e '.imageLayoutVersion == "1.0.0"' >/dev/null || \
	fail "invalid OCI layout version"

index_json=$(read_entry index.json) || fail "cannot read OCI index"
manifest_digest=$(printf '%s\n' "$index_json" | jq -er \
	'if .schemaVersion == 2 and (.manifests | length) == 1 then .manifests[0].digest else error("invalid index") end') || \
	fail "index must contain exactly one manifest"
[[ $manifest_digest == "$BLESSED_MANIFEST" ]] || \
	fail "manifest is not the blessed S5 Alpine arm64 artifact: $manifest_digest"

manifest_path=$(verify_blob "$manifest_digest" manifest) || exit 1
manifest_json=$(read_entry "$manifest_path") || fail "cannot read manifest"
config_digest=$(printf '%s\n' "$manifest_json" | jq -er \
	'if .schemaVersion == 2 and (.layers | length) == 1 then .config.digest else error("invalid manifest") end') || \
	fail "manifest must contain one config and one layer"
layer_digest=$(printf '%s\n' "$manifest_json" | jq -er '.layers[0].digest') || \
	fail "cannot read layer digest"
[[ $config_digest == "$BLESSED_CONFIG" ]] || fail "unexpected config digest: $config_digest"
[[ $layer_digest == "$BLESSED_LAYER" ]] || fail "unexpected layer digest: $layer_digest"

config_path=$(verify_blob "$config_digest" config) || exit 1
verify_blob "$layer_digest" layer >/dev/null || exit 1
config_json=$(read_entry "$config_path") || fail "cannot read config"
printf '%s\n' "$config_json" | jq -e --arg diff_id "$BLESSED_DIFF_ID" '
	.os == "linux" and
	.architecture == "arm64" and
	((.config.Entrypoint // []) == []) and
	.config.Cmd == ["/bin/sh"] and
	.rootfs.type == "layers" and
	.rootfs.diff_ids == [$diff_id]
' >/dev/null || fail "config platform, command, or rootfs contract is invalid"

printf 'archive_sha256\t%s\n' "$archive_sha"
printf 'manifest_digest\t%s\n' "$manifest_digest"
printf 'config_digest\t%s\n' "$config_digest"
printf 'layer_digest\t%s\n' "$layer_digest"
printf 'diff_id\t%s\n' "$BLESSED_DIFF_ID"
printf '%s\n' verified
