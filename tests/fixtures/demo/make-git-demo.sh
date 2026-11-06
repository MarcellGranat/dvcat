#!/usr/bin/env bash
# Build a Git + DVC copy of the demo with three finished experiments, to try the experiments view.
# Usage: DVC=/path/to/dvc ./make-git-demo.sh [DEST]     (DEST defaults to /tmp/dvc-tui-git-demo)
# The demo itself stays Git-free (a nested .git cannot be committed); this script makes a copy.
set -euo pipefail
SRC="$(cd "$(dirname "$0")" && pwd)"
DEST="${1:-/tmp/dvc-tui-git-demo}"
DVC="${DVC:-dvc}"
export DVC_NO_ANALYTICS=1
# Identity for the commit only; nothing is written to anyone's git config.
export GIT_AUTHOR_NAME=demo GIT_AUTHOR_EMAIL=demo@example.com
export GIT_COMMITTER_NAME=demo GIT_COMMITTER_EMAIL=demo@example.com

rm -rf "$DEST"
mkdir -p "$DEST"
cp -r "$SRC/src" "$SRC/data" "$SRC/dvc.yaml" "$SRC/params.yaml" "$DEST/"
rm -f "$DEST/data/prepared.csv" "$DEST/data/features.csv"
cd "$DEST"

git init -q .
# `!/data/` because some global gitignores hide data/; --temp needs the raw data committed.
printf '__pycache__/\n!/data/\n' > .gitignore
"$DVC" init -q
"$DVC" repro -q >/dev/null
git add -A
git commit -qm "baseline: demo pipeline"

# `--temp` runs each experiment in its own copy of the committed baseline, so they do not
# inherit each other's params.
"$DVC" exp run -q --temp -n lr-001 -S train.lr=0.01 -S train.epochs=2 >/dev/null
"$DVC" exp run -q --temp -n lr-005 -S train.lr=0.05 >/dev/null
"$DVC" exp run -q --temp -n lr-010 -S train.lr=0.10 -S train.epochs=8 >/dev/null
echo "git demo ready: $DEST"
