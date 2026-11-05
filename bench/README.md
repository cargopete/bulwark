# Benchmark

Recall, precision and cost per true positive over audits whose judged findings were published
after the agents' training cutoff (about June 2026), so a hit cannot come from memory.

## Cases

| Case | Source | Known H/M | Build |
|---|---|---|---|
| `cantina-2026-revert-stableswap-hooks` | Cantina competition (sponsor's fix log) | 12 | Foundry |
| `codehawks-2026-battlechain-confidence-pools` | CodeHawks competition | 3 | Foundry |
| `guardian-2026-08-3jane-lcc` | Guardian | 18 | Foundry |
| `guardian-2026-09-iris-protocol` | Guardian | 5 | Foundry |
| `nethermind-2026-09-vana-staking` | Nethermind | 9 | Foundry |
| `oz-2026-09-onchainid` | OpenZeppelin | 12 | Foundry |
| `zenith-2026-08-daimon-dao` | Zenith | 9 | Foundry |
| `electisec-2026-07-promis` | Electisec | 10 | Hardhat, via `shared/hardhat-to-foundry.sh` |
| `cantina-2026-07-sprinter-stash-contracts` | Cantina public report | 8 | Hardhat, via `shared/hardhat-to-foundry.sh` |

Earlier audit reports that sit in a repository at the audited commit are left in, because any
auditor would have had them. Firelight, Brava, Interfold and Kerne are known to qualify but need
hand setup; see ROADMAP.md.

## Layout

    cases/<case>/project/bulwark.toml   target pinned at the audited commit
    cases/<case>/project/context/       AUDIT_CONTEXT.md and KNOWN_ISSUES.md: only what auditors had
    cases/<case>/project/prebuild.sh    Hardhat cases only
    cases/<case>/expected.json          judged findings; never mounted into the run
    results/<case>-<label>.json         scored run, plus the judge's raw reply
    runs/                               copied workspaces and run logs (gitignored)
    RESULTS.md                          one row per scored run

## A blind run

`BULWARK_BENCH=1` makes the entrypoint:
- clone fresh, check out the pinned commit, and collapse the history to a single snapshot commit
  with no remote, so later fix commits are unreachable;
- deny web tools and `curl`, `wget`, `gh`, `git clone` and `git fetch` to every agent.

There are no programme assets, so there is no scope gate and no deployed-source check, and PoCs
run locally because nothing is deployed.

## Running

    scripts/bench.sh run <case>             launch one blind run
    scripts/bench.sh score <case> <label>   copy the finished workspace out and score it
    scripts/bench.sh queue [parallel]       run every unscored case, score each as it ends
    scripts/bench.sh table                  rebuild the table in RESULTS.md

`queue` is the normal way: started with `nohup` on the audit host, it keeps `parallel` runs
going (two by default), adopts runs already in progress, and labels results with the bulwark
commit it is on.

## Scoring

`bulwark bench score` collects everything the run produced: ideas agents considered and
dismissed, raw agent and fuzzer findings, merged findings, and which gates each merged finding
passed. One judge call matches each known bug to those candidates by root cause and code
location, and the furthest stage it reached is recorded:

    considered → raw → merged → past scope → past PoC → final

A column in RESULTS.md counts the known bugs that reached at least that stage, so the drop
between two columns is what that pass lost. Final findings that match no known High or Medium
are labelled by the judge as a known Low, a duplicate (a variant of a bug the run already found),
a plausible new defect, or invalid. Precision is taken over Medium-and-above final findings,
leaving out duplicates, and counts matches plus plausible new defects; judged reports rarely list
Lows, so counting them would only measure that. Cost comes from `cost.json`.

## Adding a case

A case needs a public repository at the exact audited commit, a judged report published after
the cutoff, and at least one High or three Mediums.

1. `project/bulwark.toml`: repo, branch containing the commit, commit, scope (the directory with
   `foundry.toml`), core contracts, and one `[[passes.agents.components]]` per area of the code.
   For Hardhat-only code, copy `shared/hardhat-to-foundry.sh` to `project/prebuild.sh` and set
   `pre_build_cmd = "bash /home/auditor/project/prebuild.sh"`.
2. `project/context/AUDIT_CONTEXT.md`: the README and docs at the audited commit, plus the
   in-scope file list. `KNOWN_ISSUES.md`: the publicly known issues, if any. Nothing from the
   findings report.
3. `expected.json`:

       {"case": "<case>", "source": "<report URL>",
        "findings": [{"id": "H-01", "severity": "High", "title": "...",
                      "contracts": ["src/X.sol"], "functions": ["f"],
                      "root_cause": "one or two sentences"}],
        "others": [ judged Lows, same shape, optional ]}

4. Check that it builds at the commit in a scratch container before adding it, and that nothing
   in the repository at that commit describes the findings (a later fix log, a findings file, the
   fixed code beside the audited code).
