# Common tasks. Run `just` to list them.

# Which dvc to use: $DVC_TUI_DVC, else `dvc` on PATH, else the articleIVdb virtualenv. Edit the last part for your machine.
dvc := env("DVC_TUI_DVC", `command -v dvc || echo "$HOME/projects/articleIVdb/.venv/bin/dvc"`)
demo_dir := env("DVC_TUI_DEMO_DIR", "/tmp/dvc-tui-git-demo")

default:
    @just --list

# Open the Git demo (three finished experiments). Builds it on first use.
demo:
    #!/usr/bin/env bash
    set -euo pipefail
    [ -x "{{dvc}}" ] || command -v "{{dvc}}" >/dev/null || { echo "dvc not found: {{dvc}} (set DVC_TUI_DVC)"; exit 1; }
    [ -d "{{demo_dir}}/.git" ] || DVC="{{dvc}}" tests/fixtures/demo/make-git-demo.sh "{{demo_dir}}"
    DVC_TUI_DVC="{{dvc}}" cargo run --quiet -- "{{demo_dir}}"

# Throw the Git demo away and build it again (also resets edits made while playing).
demo-reset:
    DVC="{{dvc}}" tests/fixtures/demo/make-git-demo.sh "{{demo_dir}}"

# Open the plain demo on a scratch copy; experiments are unavailable there (no Git), the pipeline works.
demo-plain:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf /tmp/dvc-tui-plain-demo
    cp -r tests/fixtures/demo /tmp/dvc-tui-plain-demo
    DVC_TUI_DVC="{{dvc}}" cargo run --quiet -- /tmp/dvc-tui-plain-demo

# Format, lint and test (the DVC-backed tests run when dvc is found).
check:
    cargo fmt
    cargo clippy --all-targets -- -D warnings
    DVC_BIN="{{dvc}}" cargo test
