#!/usr/bin/env bash
# Give a Hardhat-only package a Foundry build: npm dependencies, forge-std, and a foundry.toml
# that keeps Hardhat's compiler settings. Runs as pre_build_cmd inside the package directory.
set -euo pipefail

if [ -f package.json ] && [ ! -d node_modules ]; then
    if [ -f yarn.lock ]; then
        corepack enable >/dev/null 2>&1 || true
        yarn install --ignore-scripts --non-interactive >/dev/null 2>&1 \
            || npm install --ignore-scripts --legacy-peer-deps --no-audit --no-fund >/dev/null
    elif [ -f pnpm-lock.yaml ]; then
        pnpm install --ignore-scripts >/dev/null 2>&1 \
            || npm install --ignore-scripts --legacy-peer-deps --no-audit --no-fund >/dev/null
    else
        npm install --ignore-scripts --legacy-peer-deps --no-audit --no-fund >/dev/null
    fi
fi
[ -d lib/forge-std ] || git clone -q --depth 1 --branch v1.9.7 https://github.com/foundry-rs/forge-std lib/forge-std

[ -f foundry.toml ] && exit 0
src=contracts
[ -d "$src" ] || src=src
cfg=$(ls hardhat.config.* 2>/dev/null | head -1 || true)
via_ir=false
runs=200
if [ -n "$cfg" ]; then
    grep -qE 'viaIR:\s*true' "$cfg" && via_ir=true
    r=$(grep -oE 'runs:\s*[0-9]+' "$cfg" | head -1 | grep -oE '[0-9]+' || true)
    [ -n "$r" ] && runs=$r
fi
cat > foundry.toml <<TOML
[profile.default]
src = "$src"
test = "test-forge"
libs = ["node_modules", "lib"]
optimizer = true
optimizer_runs = $runs
via_ir = $via_ir
TOML
mkdir -p test-forge
forge remappings > remappings.txt 2>/dev/null || true
grep -q '^forge-std/' remappings.txt || echo "forge-std/=lib/forge-std/src/" >> remappings.txt
