#!/usr/bin/env bash
# Quality gates for sudobot-chat-guard. Run from anywhere; executes in the repo root.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== cargo fmt --check =="
cargo fmt --check

echo "== cargo clippy --all-targets -- -D warnings =="
cargo clippy --all-targets -- -D warnings

echo "== cargo test =="
cargo test

cat <<'EOF'

All default gates passed.

Further suites (opt-in):
  Regression tests (audit findings; FAIL by design until fixed — run individually):
    cargo test -- --ignored <name>     # see tests/README.md for the list
  Simulations (wiremock, offline):
    cargo test --test simulations
  Live API quality checks (consume real quota):
    cp .env.example .env               # fill in real keys first
    cargo test --test live_api -- --ignored --nocapture
EOF
