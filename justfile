# Common tasks. Run `just` to list them.

# Which dvc to use: $DVCAT_DVC, otherwise `dvc` from PATH.
dvc := env("DVCAT_DVC", "dvc")
demo_dir := env("DVCAT_DEMO_DIR", "/tmp/dvcat-git-demo")

default:
    @just --list

# Open the Git demo (three finished experiments). Builds it on first use.
demo:
    #!/usr/bin/env bash
    set -euo pipefail
    [ -x "{{dvc}}" ] || command -v "{{dvc}}" >/dev/null || { echo "dvc not found: {{dvc}} (set DVCAT_DVC)"; exit 1; }
    [ -d "{{demo_dir}}/.git" ] || DVC="{{dvc}}" tests/fixtures/demo/make-git-demo.sh "{{demo_dir}}"
    DVCAT_DVC="{{dvc}}" cargo run --quiet -- "{{demo_dir}}"

# Throw the Git demo away and build it again (also resets edits made while playing).
demo-reset:
    DVC="{{dvc}}" tests/fixtures/demo/make-git-demo.sh "{{demo_dir}}"

# Open the plain demo on a scratch copy; experiments are unavailable there (no Git), the pipeline works.
demo-plain:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf /tmp/dvcat-plain-demo
    cp -r tests/fixtures/demo /tmp/dvcat-plain-demo
    DVCAT_DVC="{{dvc}}" cargo run --quiet -- /tmp/dvcat-plain-demo

# Format, lint and test (the DVC-backed tests run when dvc is found).
check:
    cargo fmt
    cargo clippy --all-targets -- -D warnings
    DVC_BIN="{{dvc}}" cargo test

# Build and install dvcat from this checkout (replaces the installed one, even at the same version).
install:
    cargo install --path . --locked --force
