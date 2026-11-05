# Context Generator

The target has no hand-written audit context. Write what is missing so later passes have a
protocol description and a set of properties to check. Do not look for bugs here, and do not
report any.

Read the repository's README and docs, the deployment scripts, and the in-scope contracts listed
below. Read the contracts themselves; do not infer behaviour from names.

## AUDIT_CONTEXT.md (only if the output list below asks for it)

- What the protocol does, in a paragraph.
- Contracts in scope, one line each: role, key state, external entry points.
- Actors and trust: every privileged role, what it can do, and whether the docs treat it as
  trusted. Unprivileged users are the attacker model.
- External dependencies (oracles, tokens, other protocols) and what the code assumes of them.
- Value flows: where funds enter, where they are held, and every path that sends them out.
- Anything the README or docs call intended behaviour or a known limitation, quoted with its source.

## PROPERTIES.md (only if the output list below asks for it)

Security properties that must hold if the code is correct. Each is falsifiable by a test.

- One heading per property: `### P-<n>: <short name>`, numbered from 1.
- Under it: the statement in terms of real state variables and functions, the contracts and
  functions involved, and the tolerance where rounding applies.
- Cover: conservation of value (sums of balances against totals and holdings), access control on
  every state-changing function, state-machine transitions, rounding direction (who it may
  favour), share and price monotonicity, solvency, and liveness (a user can always exit when the
  docs say they can).
- Prefer 12 to 30 properties. A property the code plainly violates is still written as the
  intended behaviour; finding the violation is a later pass's job.

Write each requested file at the exact path given, then stop.
