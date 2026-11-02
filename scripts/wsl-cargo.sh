#!/usr/bin/env bash
# Run cargo from WSL against the Windows-hosted checkout, keeping build output on the Linux filesystem.
set -euo pipefail
export CARGO_TARGET_DIR="${MAW_TARGET_DIR:-$HOME/maw-target}"
cd "$(dirname "$0")/.."
exec cargo "$@"
