#!/usr/bin/env bash
# Everything CI checks, in the order CI checks it. Run this before pushing.
#
# Two lessons are baked in here, both learned the expensive way:
#
#   1. Checking only the crate you edited proves nothing. A field added to a
#      type in mailer-core is constructed in src-tauri; `cargo check -p
#      mailer-core` passes and CI still goes red. Hence --workspace.
#   2. The cross-platform test matrix cannot build src-tauri, because that
#      needs GTK/WebKit. Widening it to --workspace just moves the red from
#      one job to another. The workspace check belongs on the machine that has
#      the system libraries — here, and in CI's `tauri` job.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== cargo test (portable crates — mirrors the CI matrix) =="
cargo test -p mailer-core -p mailer-mcp

echo "== cargo check (whole workspace, all targets — needs GTK/WebKit) =="
cargo check --workspace --all-targets

echo "== tsc =="
npx tsc --noEmit

echo "== vite build =="
npm run build

echo
echo "all green"
