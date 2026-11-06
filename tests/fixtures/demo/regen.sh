#!/usr/bin/env bash
# Rebuild the fixture's DVC state (.dvc, outputs, dvc.lock) from scratch.
# Usage: DVC=/path/to/dvc ./regen.sh   (defaults to `dvc` on PATH)
set -euo pipefail
cd "$(dirname "$0")"
DVC="${DVC:-dvc}"
rm -rf .dvc dvc.lock model.txt metrics.json data/prepared.csv data/features.csv
"$DVC" init --no-scm -q
"$DVC" repro -q
echo "fixture ready: $(pwd)"
