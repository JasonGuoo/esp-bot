#!/usr/bin/env bash
# Run ESP-IDF commands with the project's locally installed S31 toolchain.
set -euo pipefail
firmware_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
idf_dir="${firmware_dir}/.toolchains/esp-idf"
export IDF_TOOLS_PATH="${firmware_dir}/.toolchains/idf-tools"
export IDF_COMPONENT_STORAGE_URL="${IDF_COMPONENT_STORAGE_URL:-https://components-file.espressif.com}"
if [[ ! -f "${idf_dir}/export.sh" ]]; then
    printf 'ESP-IDF is not installed at %s; see docs/esp-idf-development.md\n' "${idf_dir}" >&2
    exit 1
fi
# Espressif's export script initializes the cross compiler and Python venv.
# shellcheck disable=SC1091
source "${idf_dir}/export.sh" >&2
exec idf.py --preview -C "${firmware_dir}" "$@"
