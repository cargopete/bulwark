# Recall benchmark

Each case is a contest whose judged findings were published after the agents' training cutoff
(about June 2026), so a hit cannot come from memory.

    bench/cases/<case>/project/bulwark.toml   target repo pinned at the audited commit
    bench/cases/<case>/project/context/       AUDIT_CONTEXT.md and KNOWN_ISSUES.md from the contest README
    bench/cases/<case>/expected.json          judged findings; never mounted into the run

A bench run (`BULWARK_BENCH=1`) is blind: the clone is collapsed to one snapshot commit so later
fix commits are gone, and web tools plus curl, wget, gh and git fetch are denied.

    scripts/bench.sh run <case>
    scripts/bench.sh score <case> <label>
    scripts/bench.sh table

`expected.json`:

    {"case": "...", "source": "<judged report URL>",
     "findings": [{"id": "H-01", "severity": "High", "title": "...", "contracts": ["X.sol"],
                   "functions": ["f"], "root_cause": "one or two sentences"}],
     "others": [ same shape, for judged Lows ]}

Context files carry only what contestants had: the README, docs and publicly known issues.
Nothing from the findings report goes into `project/`.
