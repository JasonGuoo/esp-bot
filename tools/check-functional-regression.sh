#!/bin/sh
# Local supplementary suite. Never invokes GitHub CI, flashes, or installs models.
set -eu
repo_dir=$(cd "$(dirname "$0")/.." && pwd)
exec "${ESPBOT_REGRESSION_PYTHON:-python3}" "$repo_dir/tests/regression/run.py" "$@"
