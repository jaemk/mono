#!/usr/bin/env bash
# bin/test-js.sh
#
# Run the komino client tests (jsdom) with coverage thresholds, as CI does.
#
# Finds node on PATH, then under nvm (~/.nvm), and otherwise runs the suite
# in a throwaway node docker container, so no manual setup is needed.
# Override the container image with TEST_NODE_IMAGE (default node:24).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WEB="${ROOT}/crates/komino/web"

if ! command -v npm >/dev/null 2>&1 && [ -s "${NVM_DIR:-${HOME}/.nvm}/nvm.sh" ]; then
    # shellcheck disable=SC1091
    set +u; source "${NVM_DIR:-${HOME}/.nvm}/nvm.sh" >/dev/null; set -u
    nvm use --silent default >/dev/null 2>&1 || true
fi

if command -v npm >/dev/null 2>&1; then
    echo "==> Running komino client tests with $(command -v node) ($(node --version))"
    builtin cd "${WEB}"
    npm ci --no-audit --no-fund --loglevel=error
    npm run coverage
elif command -v docker >/dev/null 2>&1; then
    image="${TEST_NODE_IMAGE:-node:24}"
    echo "==> No node found; running komino client tests in '${image}'"
    # the assets the tests load sit beside web/, so mount the whole crate;
    # node_modules stays inside the container
    docker run --rm -v "${ROOT}/crates/komino:/komino:ro" -w /komino/web "${image}" \
        sh -c 'cp -r /komino /tmp/komino && cd /tmp/komino/web && npm ci --no-audit --no-fund --loglevel=error && npm run coverage'
else
    echo "ERROR: node is not installed (on PATH or via nvm) and docker is not" >&2
    echo "       available to run it in a container." >&2
    exit 1
fi
