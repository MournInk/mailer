#!/usr/bin/env bash
# Everything CI checks, in the order CI checks it.
#
# CI once went red three commits running because a field was added to a type in
# mailer-core and verified with `cargo check -p mailer-core` — which passes,
# because the crate that constructs that struct is src-tauri. Checking only the
# crate you edited proves nothing about the workspace. Run this before pushing.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== cargo check (workspace, all targets) =="
cargo check --workspace --all-targets

echo "== cargo test (workspace) =="
cargo test --workspace

echo "== tsc =="
npx tsc --noEmit

echo "== vite build =="
npm run build

echo
echo "all green"
