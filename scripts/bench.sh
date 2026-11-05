#!/usr/bin/env bash
# Recall benchmark runner. Run from the bulwark repo root on the machine that runs audits.
#
#   scripts/bench.sh run   <case>           launch a blind run of bench/cases/<case>
#   scripts/bench.sh score <case> <label>   copy the finished workspace out and score it
#   scripts/bench.sh table                  rebuild bench/RESULTS.md from bench/results/
#
# A case is bench/cases/<case>/project/ (bulwark.toml + context/, mounted into the run) and
# bench/cases/<case>/expected.json (the judged findings, never mounted into the run).
set -euo pipefail

cmd=${1:-}
case_id=${2:-}
root=$(pwd)
image=bulwark-audit-env

repo_name() {
    grep -m1 '^repo = ' "bench/cases/$1/project/bulwark.toml" | sed 's/.*"\(.*\)".*/\1/' \
        | xargs basename | sed 's/\.git$//'
}

case "$cmd" in
run)
    [ -d "bench/cases/$case_id/project" ] || { echo "no case $case_id"; exit 1; }
    name=$(repo_name "$case_id")
    docker run --rm -v bulwark_audit-outputs:/v alpine rm -rf "/v/$name"
    BULWARK_PROJECT="$root/bench/cases/$case_id/project" docker compose run -d \
        --name "bench-$case_id" -e ANTHROPIC_API_KEY= -e BULWARK_BENCH=1 audit-env bulwark run
    ;;
score)
    label=${3:?label, e.g. the bulwark commit}
    name=$(repo_name "$case_id")
    dest="bench/runs/$case_id/$label"
    mkdir -p "$dest"
    docker run --rm -v bulwark_audit-outputs:/v:ro -v "$root/$dest:/out" alpine \
        sh -c "cd /v/$name/audit-workspace && tar cf - --exclude='*/out' --exclude='*/cache' . | tar xf - -C /out"
    docker run --rm --entrypoint /usr/local/bin/bulwark \
        -v "$root/bench:/bench" \
        -v "$HOME/.claude/.credentials.json:/home/auditor/.claude/.credentials.json" \
        "$image" bench score --expected "/bench/cases/$case_id/expected.json" \
        --workspace "/bench/runs/$case_id/$label" --out /bench/results --label "$label"
    ;;
table)
    docker run --rm --entrypoint /usr/local/bin/bulwark -v "$root/bench:/bench" "$image" \
        bench table --results /bench/results > bench/RESULTS.md.table
    { sed '/^| run |/,$d' bench/RESULTS.md; cat bench/RESULTS.md.table; } > bench/RESULTS.md.new
    mv bench/RESULTS.md.new bench/RESULTS.md && rm bench/RESULTS.md.table
    ;;
*)
    sed -n '2,9p' "$0"; exit 1 ;;
esac
