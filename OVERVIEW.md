# Bulwark: design

The [README](README.md) says what each pass does. This document says why it is built that way.

## The job

Bug bounty programmes pay the first valid report of an in-scope, demonstrable, previously
unknown bug, at a severity set by the programme's impact table. Most of what an AI audit
produces fails one of those tests: it is out of scope, already disclosed, not demonstrable, or
rated by the model rather than by the table. Bulwark is built to remove those before a person
spends time on them, and to say plainly what it could not establish.

It does not write submissions. Immunefi bans AI-generated reports, and a report is only as good
as the proof a person has re-run. The pipeline's output is leads plus scaffolding.

## Principles

**Silence is not a result.** A pass that ran nothing must fail, because its empty output reads
as "the code is clean". Most of the faults found in live runs (see [ROADMAP](ROADMAP.md)) were of
this shape: tests forge never collected, invariants that never compiled, solver budgets of 300
milliseconds. Each fix turned a quiet success into a loud failure.

**Proof is measured, not asserted.** A PoC proves impact only by logging a nonzero attacker gain
or victim loss. A test that merely passes proves nothing, and a retry is never told to "make the
test pass", which would coach it into weakening its assertions. Cheatcodes that hand an attacker
powers it does not have (pranking a governor, dealing to a protocol contract, writing storage) are
flagged, and attack capital is declared.

**The deployed code is the target.** PoCs fork the pinned block and attack real addresses, and
Pass 1 checks that the pinned commit is what the proxies actually route to. On SSV an hour of
agent work went into code the proxy had already replaced.

**Severity belongs to the programme.** Findings map to one asset and one impact row. The
reviewer may move severity only to a row in the table, and the reward estimate comes from the
programme's own ceilings.

**Known is worth zero.** Published audits and known issues are ingested into `known/`, and a
finding that matches one is dropped before any PoC budget is spent on it.

**Agents are held to evidence.** A session must show it read its files in full, wrote down
hypotheses and ran tests. A short session is resumed with the gap named rather than accepted.

**Measure before tuning.** Without recall figures there is no way to tell whether a prompt or
model change helped. The benchmark runs blind cases whose findings postdate the model's training,
and traces each known bug through every stage, so a miss names the pass that lost it.

## What is deterministic and what is a model

| Step | Deterministic | Model |
|---|---|---|
| Deployed-source check, build, Slither, layouts, entry points | yes | |
| Derived AUDIT_CONTEXT.md / PROPERTIES.md when missing | | yes |
| Agent analysis, hunters, variant analysis | | yes |
| Evidence gate (what was read, tested, written) | yes | |
| Scope gate | rules first | classifier for the rest |
| Known-issue matching | | yes, verdict read from a file |
| PoC writing | | yes |
| PoC grading (impact markers, cheatcode lint) | yes | |
| Invariant suite writing, failure classification | | yes, verdict read from a file |
| Forge, Medusa, Echidna, Halmos | yes | |
| Adversarial review | | yes, severity bounded by the impact table |
| Report and submission drafts | yes, from recorded data | quoted model text only |

Where a model's verdict decides something, it writes the verdict to a file the pipeline parses,
not free text the pipeline searches. A sentence such as "this is not a duplicate" once matched
a search for "duplicate".

## Data flow

Every pass reads and writes JSON in the workspace. Nothing passes between passes in a prompt
alone, so any stage can be re-run, inspected or scored. A finding keeps one ID from the merge
onwards, and its PoC record, gate decisions and review challenges are all keyed by it. That is
what lets the benchmark say where a bug was lost and lets `bulwark findings --rejected` show
what each gate threw away.

## Two layers of formal verification

Halmos on whole contracts is expensive and often needs mocks. Halmos on hand-copied arithmetic is
cheap, but it proves things about the copy. Bulwark runs both and labels them: `real` checks run
the in-scope bytecode, and `model` checks run the copies. Neither kind of VERIFIED clears a
finding, because a bounded proof over a model says little about a deployed system. A real-layer
counterexample becomes a finding; a model-layer one is a hint.

## One suite, three fuzzers

Forge, Medusa and Echidna disagree on almost everything: how setup runs, how properties report,
which cheatcodes exist. The generated suite is written to their common ground. Setup lives in a
constructor, properties return `bool`, the only cheatcodes are prank, warp and roll, and a thin
Forge entry asserts each property. One set of properties then gets three different search
strategies, and a broken invariant is classified before it is believed, because most broken
generated invariants are wrong invariants.

## History

Bulwark started as an audit of The Graph's Horizon contracts, and for a while its prompts, schema
and default context still assumed Graph property numbers. The last of those assumptions were
removed in November 2026; results reported from that period came from passes later found to
have run nothing, and are not repeated here.
