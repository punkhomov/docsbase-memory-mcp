# Integrity

Keeps the design process honest. Apply before presenting any final claim, and whenever
something feels "obvious but unverified".

## System 2 reflective loop

Before output, run one explicit reflection pass:

```
1. Draft        — produce the answer / section.
2. STOP         — do not emit yet.
3. Reflect      — Did I hallucinate? Did I assume? Is there a contradiction?
4. Verify       — check with a tool: read files, grep, run the command, fetch the docs.
5. Output       — only the verified result.
```

Never skip step 3 for claims about paths, APIs, crate names, flags, or "there is no X".

## Silent integrity check

Run this internally on every artifact before presenting it:

- **Contradictions** — does this statement conflict with an earlier requirement, the
  spec, or the code? Surface it, do not paper over it.
- **Hidden assumptions** — list every assumption the design relies on and mark it.
- **Leverage points** — where does one small decision change many others? Flag those.
- **Data flows** — trace each input to its output; find gaps and dead ends.
- **Hidden dependencies** — implicit coupling between components, order dependencies,
  shared mutable state.
- **Unstated constraints** — performance, memory, platform, licensing, offline.

Present findings as a short list, not prose. If a contradiction blocks progress, stop and
ask which side wins.

## Anti-rationalization guardrail

Every design artifact should be able to answer:

- **When NOT to use this approach** — the honest boundary conditions.
- **Common rationalizations + rebuttals** — e.g. "we can optimize later" → but the data
  model choice may make it impossible; state the cost.
- **Red flags** — signals that the design is wrong (unbounded growth, global mutable
  state, sync-over-async, hidden network calls).
- **Exit criteria** — what must be true for the phase to count as done.

If the user pushes for speed, do not silently drop verification. Say what is being skipped
and what risk it carries.

## Red-team vectors (challenger)

Run all five against the current artifact **at every gate**, before asking for approval.
These are the adversarial questions that find what normal review misses:

1. **Unstated** — what did we *not* say? Missing NFRs, implicit users, assumed inputs,
   undefined behavior on the empty/error case.
2. **Incentive** — who benefits from this being accepted, and what is being optimized at
   the expense of what? (e.g. speed over correctness, simplicity over safety.)
3. **Adversarial** — how would a hostile actor abuse this? Injection from untrusted docs,
   path escape, resource exhaustion, data leakage, supply-chain surface.
4. **Scale-break** — what happens at 10× / 100× data, files, users, or size? Where is the
   first cliff, and is the failure graceful?
5. **Sunset** — how does this get removed or migrated? What happens when a dependency dies,
   a format changes, or the feature becomes obsolete?

Output a short ranked list of findings with severity and a **recommended default** for each.
Do not just list problems — propose the resolution, then ask only if it is a product-level
trade-off.

## Traceability verification

- Every design element maps to a requirement ID. Run the reverse check too: every
  requirement is covered by design and by at least one task.
- Verify claims with grep/read instead of memory: file paths, symbol names, config keys.
- When stating "X is not used / does not exist", show the search that proves it.

## Consistency audit

Run this **at every gate**, not once at the end. Check cross-artifact alignment:

```text
requirements  ↔  design  ↔  tasks
```

Mismatches to look for: requirement without design, design without task, task with no
requirement (gold-plating), terms used inconsistently, version/name drift.

## Spec self-review (before approval)

With fresh eyes, scan the artifact itself:

1. **Placeholder scan** — any `TBD`, `TODO`, `[NEEDS CLARIFICATION]`, or vague requirement?
   Resolve it or state the default explicitly.
2. **Internal consistency** — does any section contradict another? Does the architecture
   match the feature list?
3. **Scope check** — focused enough for a single plan, or does it need decomposition?
4. **Ambiguity check** — could a requirement be read two ways? Pick one reading and make
   it explicit in the text.

## Conflict resolution → ADR

When two requirements or constraints conflict:

1. State both sides exactly.
2. List options with trade-offs.
3. Recommend one; ask the user to decide if it is a product-level trade-off.
4. Record the decision as a short ADR in `design.md` (context, decision, consequences).

## Quality gate

Before marking a document approved, score it:

| Dimension | Question |
|---|---|
| Completeness | All required sections present and filled? |
| Specificity | Zero generic filler; names and paths are real? |
| Testability | Acceptance criteria are observable? |
| Traceability | Requirements ↔ design ↔ tasks all linked? |
| Risk awareness | Trade-offs and failure modes named? |
| Consistency | No contradictions inside or across documents? |

Verdict: **PASS** / **CONDITIONAL** (list what must be fixed) / **REJECT** (restart the
phase). Do not advance on CONDITIONAL until the listed items are resolved.

## Verification before completion (execution)

**Iron law: no completion claim without fresh verification evidence.**

```
BEFORE claiming any status:
1. IDENTIFY — what command proves this claim?
2. RUN      — execute the full command, fresh
3. READ     — full output, exit code, failure count
4. VERIFY   — does the output confirm the claim?
5. CLAIM    — only now, and state the claim together with the evidence
```

| Claim | Requires | Not sufficient |
|---|---|---|
| Tests pass | test output: 0 failures | a previous run, "should pass" |
| Lint/typecheck clean | the actual command output | extrapolation |
| Build succeeds | build command exit 0 | linter passing |
| Bug fixed | the original symptom now passes | code changed |
| Task complete | verify command output | "looks done" |

Red flags — stop if you catch yourself using `should`, `probably`, `seems`, or expressing
satisfaction before running the command. Rationalizations ("I'm confident", "just this
once", "the linter already passed") are not evidence.

## Convergence

After execution (or at the end of a review pass), verify the delivered result against the
artifacts, line by line:

- every `FR-*` implemented or explicitly deferred;
- every `SC-*` demonstrably met;
- every task accounted for;
- code matches the design's interfaces, or the design was updated.

Report a verdict with evidence:

- **converged** — all requirements met, no drift;
- **partial** — list exactly what is missing or drifted;
- **failed** — implementation does not satisfy the spec; explain the divergence.

Never mark a phase converged on the strength of "tests pass" alone — requirements coverage
is checked separately.
