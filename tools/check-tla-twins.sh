#!/bin/sh
# Run every registered TLA+ twin, or a single owner/model, on the development Mac.
set -eu
repo_dir=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_dir"

if [ "$#" -eq 0 ]; then
    set -- formal/tla/*/*/
elif [ "$#" -eq 1 ] && [ "$1" != "${1#*/}" ]; then
    case "$1" in *..*|/*|*/*/*) echo "Expected owner/Model" >&2; exit 2 ;; esac
    set -- "formal/tla/$1"
else
    echo "Usage: $0 [owner/Model]" >&2
    exit 2
fi
for twin in "$@"; do
    model=$(basename "$twin")
    for file in "$model.tla" "$model.cfg" checks.sh; do
        [ -f "$twin/$file" ] || { echo "Missing twin file: $twin/$file" >&2; exit 2; }
    done
done

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

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Library/Developer/CommandLineTools}"
rust_tests() {
    manifest=$1 suite=$2
    shift 2
    if ! cargo test --offline --locked --manifest-path "$manifest" "$suite" -- "$@" \
        > "$scratch/rust-tests.log" 2> "$scratch/cargo.log"; then
        cat "$scratch/cargo.log" "$scratch/rust-tests.log" >&2
        return 1
    fi
    cat "$scratch/rust-tests.log"
    for filter do
        if ! awk -v term="$filter" '$1 == "test" && $3 == "..." && $4 == "ok" && index($2, term) { ran=1 } END { exit !ran }' "$scratch/rust-tests.log"; then
            echo "No Rust regression ran for $filter" >&2
            return 1
        fi
    done
}
server_tests() {
    rust_tests server/bot-server/src-tauri/Cargo.toml --lib "$@"
}
for twin in "$@"; do
    twin=${twin%/}
    model=$(basename "$twin")
    echo "Checking $twin"
    mkdir -p "$scratch/$twin"
    java -XX:+UseParallelGC -Xmx1g -cp "$jar" tlc2.TLC -workers 1 \
        -metadir "$scratch/$twin/states" -config "$twin/$model.cfg" "$twin/$model.tla"
    ( . "./$twin/checks.sh" )
done
