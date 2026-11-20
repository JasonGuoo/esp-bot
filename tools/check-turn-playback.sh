#!/bin/sh
# Check the playback contract and its firmware and gateway implementations on Mac.
set -eu
repo_dir=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_dir"

version=1.7.4
sha256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
cache_dir="${XDG_CACHE_HOME:-$HOME/Library/Caches}/esp-bot/tlaplus/$version"
mkdir -p "$cache_dir"
scratch=$(mktemp -d "$cache_dir/check.XXXXXXXX")
trap 'rm -rf "$scratch"' EXIT
jar="$cache_dir/tla2tools.jar"

verify_jar() {
    [ -f "$1" ] && printf '%s  %s\n' "$sha256" "$1" | shasum -a 256 --check --status
}

if ! verify_jar "$jar"; then
    echo "Downloading TLA+ Tools $version"
    curl --fail --location --silent --show-error --retry 2 \
        "https://github.com/tlaplus/tlaplus/releases/download/v$version/tla2tools.jar" \
        --output "$scratch/tla2tools.jar"
    if ! verify_jar "$scratch/tla2tools.jar"; then
        echo "TLA+ Tools $version checksum mismatch" >&2
        exit 1
    fi
    mv -f "$scratch/tla2tools.jar" "$jar"
fi

spec=docs/specs/household-companion/tla
java -XX:+UseParallelGC -Xmx1g -cp "$jar" tlc2.TLC -workers 1 \
    -metadir "$scratch/states" -config "$spec/TurnPlayback.cfg" "$spec/TurnPlayback.tla"
firmware/components/bot_runtime/tests/run-host-macos.sh
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Library/Developer/CommandLineTools}"
cargo test --offline --locked --manifest-path server/bot-server/src-tauri/Cargo.toml --lib brain::gateway::
