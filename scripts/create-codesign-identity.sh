#!/usr/bin/env bash
# Compatibility entry point for the canonical local signing provisioner.
set -euo pipefail
exec bash "$(dirname "$0")/codesign-identity.sh" "$@"
