#!/usr/bin/env bash
set -euo pipefail

version=${1:?usage: package-release.sh VERSION [BINARY] [OUTPUT_DIRECTORY]}
binary=${2:-target/release/efuseek}
output=${3:-dist}
expected=$(cargo metadata --locked --offline --no-deps --format-version 1 |
  jq -r '.packages[] | select(.name == "efuseek") | .version')
[[ "$version" == "$expected" ]] || { printf 'Version must match Cargo.toml: %s\n' "$expected" >&2; exit 1; }
[[ -f "$binary" ]] || { printf 'Missing release binary: %s\n' "$binary" >&2; exit 1; }
[[ $(uname -m) == x86_64 ]] || { printf 'This package currently targets Linux x86_64.\n' >&2; exit 1; }

package="efuseek-v${version}-linux-x86_64"
stage="$output/$package"
[[ ! -e "$stage" && ! -e "$output/$package.tar.gz" ]] || { printf 'Refusing to overwrite an existing package.\n' >&2; exit 1; }
mkdir -p "$stage/bin" "$stage/share/efuseek" "$stage/licenses/third-party"
install -m 755 "$binary" "$stage/bin/efuseek"
install -Dm644 data/io.github.efuseek.EfuSeek.desktop "$stage/share/applications/io.github.efuseek.EfuSeek.desktop"
install -m 644 config.default.toml config.example.toml "$stage/share/efuseek/"
install -m 644 README.md CHANGELOG.md LICENSE "$stage/"

# Preserve dependency notices without including local registry paths or metadata JSON.
metadata=$(mktemp "$output/.cargo-metadata.XXXXXX")
trap 'rm -f -- "$metadata"' EXIT
cargo metadata --locked --format-version 1 > "$metadata"
{
  printf '# Third-party Rust dependency notices\n\n'
  printf 'Dependencies retain their own licenses; this list also includes development and target-specific dependencies.\n'
  printf 'GTK, libadwaita and GLib runtime libraries are provided by the operating system, not bundled.\n\n'
  printf '| Package | Version | Declared license |\n| --- | --- | --- |\n'
  jq -r '.packages[] | select(.name != "efuseek") | "| \(.name) | \(.version) | \(.license // "See package notices") |"' "$metadata"
} > "$stage/THIRD_PARTY_NOTICES.md"
shopt -s nullglob
while IFS=$'\t' read -r name dependency_version manifest; do
  directory=${manifest%/*}
  notices=("$directory"/LICENSE* "$directory"/LICENCE* "$directory"/COPYING* "$directory"/NOTICE* "$directory"/COPYRIGHT* "$directory"/license*)
  for notice in "${notices[@]}"; do
    if [[ -f "$notice" ]]; then
      install -Dm644 "$notice" "$stage/licenses/third-party/$name-$dependency_version/${notice##*/}"
    fi
  done
done < <(jq -r '.packages[] | select(.name != "efuseek") | [.name, .version, .manifest_path] | @tsv' "$metadata")
tar -czf "$output/$package.tar.gz" -C "$output" "$package"
(cd "$output" && sha256sum "$package.tar.gz" > SHA256SUMS)
printf 'Created %s/%s.tar.gz\n' "$output" "$package"
