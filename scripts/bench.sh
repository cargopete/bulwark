#!/usr/bin/env bash
# Recall benchmark runner. Run from the bulwark repo root on the machine that runs audits.
#
#   scripts/bench.sh run   <case>           launch a blind run of bench/cases/<case>
#   scripts/bench.sh score <case> <label>   copy the finished workspace out and score it
#   scripts/bench.sh table                  rebuild bench/RESULTS.md from bench/results/
#   scripts/bench.sh queue [parallel]       run every unscored case, score each as it ends
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
queue)
    # Unattended: keeps `parallel` runs going, scores each as its container exits, then
    # rebuilds the table. Runs already going (started by `run`) are adopted.
    parallel=${2:-2}
    label=$(git rev-parse --short HEAD)
    mkdir -p bench/runs bench/results
    scored() { ls "bench/results/$1-$label.json" >/dev/null 2>&1; }
    log() { echo "$(date '+%F %T') $*"; }
    while :; do
        for c in $(docker ps -a --filter name=^bench- --filter status=exited --format '{{.Names}}'); do
            id=${c#bench-}
            code=$(docker inspect -f '{{.State.ExitCode}}' "$c")
            if [ "$code" -ge 128 ]; then
                # Stopped by a signal: not a result. Removing it puts the case back in the queue.
                log "not scoring $id (killed, exit $code)"
            elif [ -d "bench/cases/$id" ] && ! scored "$id"; then
                log "scoring $id (exit $code)"
                docker logs "$c" > "bench/runs/$id-$label.log" 2>&1 || true
                "$0" score "$id" "$label" || log "scoring $id failed"
            fi
            docker rm "$c" >/dev/null
        done
        running=$(docker ps --filter name=^bench- --format '{{.Names}}' | wc -l)
        pending=""
        for d in bench/cases/*/; do
            id=$(basename "$d")
            scored "$id" || docker ps -a --format '{{.Names}}' | grep -qx "bench-$id" || pending="$pending $id"
        done
        if [ "$running" -eq 0 ] && [ -z "$pending" ]; then
            "$0" table && log "all cases scored; table rebuilt"
            break
        fi
        for id in $pending; do
            [ "$running" -ge "$parallel" ] && break
            log "starting $id"
            "$0" run "$id" >/dev/null
            running=$((running + 1))
        done
        sleep 60
    done
    ;;
*)
    sed -n '2,10p' "$0"; exit 1 ;;
esac
