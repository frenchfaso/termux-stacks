#!/data/data/com.termux/files/usr/bin/bash
# shellcheck shell=bash

set -u
set -o pipefail

readonly BASE_DIFF_ID=sha256:b2848c02ac6ff53d265469b5b30f649f335e546a83330cd8916d54e65e640409
readonly FIXTURE_REVISION=2

# These were the last reviewed revision-1 manifests. Revision 2 changes the
# worker protocol, so they are evidence of what was superseded, not values that
# may authorize a device run.
readonly SUPERSEDED_MANIFEST_V1_SHA256=be6828cb0c20d3f37b6161f11818e1fd2542e0fa403f35a4af3cc513f64097ac
readonly SUPERSEDED_MANIFEST_V2_SHA256=284bf5b40b1a54e9940f496170598f8d5b26a5f2d232cd43b68b5d4f87a8da9e

# Reviewed revision-2 manifests. Both values must change together whenever the
# fixture worker, Containerfile, or build contract changes.
readonly BLESSED_MANIFEST_V1_SHA256=e109d20537180d5b8d8d1f346a7573e2c417f502de7b590cd1df02a077744c5e
readonly BLESSED_MANIFEST_V2_SHA256=0fa8687a5d0607ff25804c2e7a67da8439f4af2990868ecc29677ca0b0ceec77

fail() {
	printf 'verify-oci-g2: %s\n' "$*" >&2
	exit 1
}

if (($# != 3)); then
	printf 'Usage: %s ABSOLUTE_OCI_ARCHIVE ARCHIVE_SHA256 VERSION\n' \
		"${0##*/}" >&2
	exit 2
fi

for command_name in tar gzip jq sha256sum; do
	command -v "$command_name" >/dev/null 2>&1 || \
		fail "required command is not installed: $command_name"
done

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P) || \
	fail "cannot resolve fixture directory"
worker=$script_dir/worker
archive=$1
expected_archive_sha=$2
expected_version=$3

[[ -f $worker && ! -L $worker ]] || fail "local worker is not a regular file"
[[ $archive == /* && -f $archive && ! -L $archive ]] || \
	fail "archive must be an absolute regular non-symlink file"
[[ $expected_archive_sha =~ ^[0-9a-f]{64}$ ]] || \
	fail "archive SHA-256 must contain 64 lowercase hexadecimal characters"
[[ $expected_version == v1 || $expected_version == v2 ]] || \
	fail "version must be v1 or v2"

if [[ $BLESSED_MANIFEST_V1_SHA256 == REBUILD_REQUIRED || \
	$BLESSED_MANIFEST_V2_SHA256 == REBUILD_REQUIRED ]]; then
	fail "fixture revision $FIXTURE_REVISION changed the worker bytes; rebuild and review both OCI fixtures, then replace both BLESSED_MANIFEST_*_SHA256 sentinels (superseded v1=$SUPERSEDED_MANIFEST_V1_SHA256, v2=$SUPERSEDED_MANIFEST_V2_SHA256)"
fi
for trusted_manifest in "$BLESSED_MANIFEST_V1_SHA256" "$BLESSED_MANIFEST_V2_SHA256"; do
	[[ $trusted_manifest =~ ^[0-9a-f]{64}$ ]] || \
		fail "a blessed manifest trust root is not a lowercase SHA-256"
done
[[ $BLESSED_MANIFEST_V1_SHA256 != "$BLESSED_MANIFEST_V2_SHA256" ]] || \
	fail "v1 and v2 blessed manifest trust roots must differ"
for trusted_manifest in "$BLESSED_MANIFEST_V1_SHA256" "$BLESSED_MANIFEST_V2_SHA256"; do
	[[ $trusted_manifest != "$SUPERSEDED_MANIFEST_V1_SHA256" && \
		$trusted_manifest != "$SUPERSEDED_MANIFEST_V2_SHA256" ]] || \
		fail "a superseded revision-1 manifest cannot authorize fixture revision $FIXTURE_REVISION"
done
if [[ $expected_version == v1 ]]; then
	expected_manifest_sha=$BLESSED_MANIFEST_V1_SHA256
else
	expected_manifest_sha=$BLESSED_MANIFEST_V2_SHA256
fi

archive_sha=$(sha256sum -- "$archive") || fail "cannot hash archive"
archive_sha=${archive_sha%% *}
[[ $archive_sha == "$expected_archive_sha" ]] || \
	fail "archive SHA-256 mismatch: expected $expected_archive_sha, got $archive_sha"
worker_sha=$(sha256sum -- "$worker") || fail "cannot hash local worker"
worker_sha=${worker_sha%% *}
listing=$(tar -tf "$archive") || fail "cannot list archive"

require_single_entry() {
	local target=$1 count
	count=$(printf '%s\n' "$listing" | awk -v target="$target" \
		'$0 == target { count += 1 } END { print count + 0 }') || \
		fail "cannot count archive entry: $target"
	[[ $count -eq 1 ]] || fail "archive must contain exactly one $target entry"
}

read_entry() {
	tar -xOf "$archive" "$1" || fail "cannot read archive entry: $1"
}

verify_blob() {
	local digest=$1 label=$2 hex path actual
	[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || \
		fail "$label does not use a valid SHA-256 digest"
	hex=${digest#sha256:}
	path=blobs/sha256/$hex
	require_single_entry "$path"
	actual=$(read_entry "$path" | sha256sum) || fail "cannot hash $label"
	actual=${actual%% *}
	[[ $actual == "$hex" ]] || fail "$label content does not match its digest"
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
		*) fail "unsupported layer media type: $media_type" ;;
	esac
}

require_single_entry oci-layout
require_single_entry index.json
read_entry oci-layout | jq -e '.imageLayoutVersion == "1.0.0"' >/dev/null || \
	fail "invalid OCI layout version"

index_json=$(read_entry index.json) || fail "cannot read OCI index"
manifest_digest=$(printf '%s\n' "$index_json" | jq -er '
	if .schemaVersion == 2 and (.manifests | length) == 1
	then .manifests[0].digest else error("invalid index") end
') || fail "index must contain exactly one manifest"
[[ $manifest_digest == "sha256:$expected_manifest_sha" ]] || \
	fail "manifest mismatch: expected sha256:$expected_manifest_sha, got $manifest_digest"
manifest_path=$(verify_blob "$manifest_digest" manifest) || exit 1
manifest_json=$(read_entry "$manifest_path") || fail "cannot read manifest"
printf '%s\n' "$manifest_json" | jq -e '
	.schemaVersion == 2 and
	(.config.digest | type == "string") and
	(.layers | type == "array" and length == 2) and
	(all(.layers[]; (.digest | type == "string") and (.mediaType | type == "string")))
' >/dev/null || fail "manifest config or layers do not match the fixture shape"

config_digest=$(printf '%s\n' "$manifest_json" | jq -er '.config.digest') || \
	fail "cannot read config digest"
config_path=$(verify_blob "$config_digest" config) || exit 1
config_json=$(read_entry "$config_path") || fail "cannot read config"
printf '%s\n' "$config_json" | jq -e \
	--arg base "$BASE_DIFF_ID" --arg version "G2_IMAGE_VERSION=$expected_version" \
	--arg revision "G2_FIXTURE_REVISION=$FIXTURE_REVISION" '
	.os == "linux" and .architecture == "arm64" and
	.config.Entrypoint == ["/g2-worker"] and
	(((.config | has("Cmd")) == false) or .config.Cmd == null or .config.Cmd == []) and
	(.config.Env | type == "array") and
	([.config.Env[] | select(startswith("G2_IMAGE_VERSION="))] == [$version]) and
	([.config.Env[] | select(startswith("G2_FIXTURE_REVISION="))] == [$revision]) and
	.rootfs.type == "layers" and
	(.rootfs.diff_ids | type == "array" and length == 2 and .[0] == $base and
		 all(.[]; test("^sha256:[0-9a-f]{64}$")))
' >/dev/null || fail "platform, Entrypoint, version, or base rootfs is not the G2 contract"

declare -a actual_diff_ids=()
layer_number=0
while IFS=$'\t' read -r layer_digest media_type; do
	[[ -n $layer_digest && -n $media_type ]] || fail "empty layer descriptor"
	layer_label="layer $((layer_number + 1))"
	path=$(verify_blob "$layer_digest" "$layer_label") || exit 1
	config_diff_id=$(printf '%s\n' "$config_json" | \
		jq -er --argjson index "$layer_number" '.rootfs.diff_ids[$index]') || \
		fail "cannot read $layer_label diff ID from config"
	diff_line=$(layer_stream "$path" "$media_type" | sha256sum) || \
		fail "cannot hash uncompressed $layer_label"
	actual_diff_id=sha256:${diff_line%% *}
	[[ $actual_diff_id == "$config_diff_id" ]] || \
		fail "$layer_label diff ID mismatch: config has $config_diff_id, content has $actual_diff_id"
	actual_diff_ids+=("$actual_diff_id")
	layer_number=$((layer_number + 1))
	if ((layer_number == 2)); then
		archived_worker_sha=$(layer_stream "$path" "$media_type" | \
			tar -xOf - g2-worker | sha256sum) || \
			fail "cannot extract worker from final layer"
		archived_worker_sha=${archived_worker_sha%% *}
	fi
done < <(printf '%s\n' "$manifest_json" | jq -er '.layers[] | [.digest, .mediaType] | @tsv')
[[ $layer_number -eq 2 ]] || fail "fixture must contain exactly two layers"
[[ ${actual_diff_ids[0]} == "$BASE_DIFF_ID" ]] || \
	fail "decompressed base layer does not match the pinned base diff ID"
[[ ${archived_worker_sha:-} == "$worker_sha" ]] || \
	fail "archived worker does not match the reviewed repository fixture"

printf 'archive_sha256\t%s\n' "$archive_sha"
printf 'manifest_digest\t%s\n' "$manifest_digest"
printf 'config_digest\t%s\n' "$config_digest"
printf 'layer_1_diff_id\t%s\n' "${actual_diff_ids[0]}"
printf 'layer_2_diff_id\t%s\n' "${actual_diff_ids[1]}"
printf 'worker_sha256\t%s\n' "$worker_sha"
printf 'version\t%s\n' "$expected_version"
printf 'fixture_revision\t%s\n' "$FIXTURE_REVISION"
printf '%s\n' verified
