# Execution

Discipline for the EXECUTION and CONVERGENCE phases. The plan already did the thinking:
execute it exactly, prove each step, and leave a record that survives context loss.

## Execution modes

Choose the mode when execution starts (`/implement --manual` or `/implement --machine`).

| Mode | Review of each task | Human involvement | When |
|---|---|---|---|
| **manual** (default) | the human, after each task | stop after every task | design uncertainty, small runs, no subagent tool |
| **machine** | a fresh reviewer subagent per task | plan approval + four stop conditions + final review | well-specified plan, independent tasks, subagent tool available |

Both modes share the ledger, so the mode can change mid-run without losing progress. If no
subagent/Task tool is available, machine mode degrades to manual — say so, do not pretend
to review.

## Ledger (survives compaction)

Conversation memory does not survive compaction. Track progress in a file, not only in
todos or chat:

- Location: `docs/specs/<slug>/progress.md`, first line naming the spec/plan.
- Append one line per task, in the same step as the commit.
- Formats:
  - `Task <N>: complete (commits <base7>..<head7>, tests: <cmd> → <result>)`
  - `Task <N>: Ruling: <what> — <why> — <cost if wrong>`
  - `Task <N>: minor (deferred): <one-liner>`

After compaction, trust the ledger and `git log` over recollection. A task with a
`complete` line is done — never redo it.

## TDD loop (per task)

**Iron law: no production code without a failing test first.** If code exists before its
test, delete it and restart from the test. No "keep as reference".

1. **RED** — one minimal test: clear name, real behavior, one thing. "And" in the name →
   split it.
2. **Verify RED** — run it and watch it fail **for the expected reason** (feature missing,
   not a typo). A test that passes immediately proves nothing; fix the test.
3. **GREEN** — the minimal code that passes. No extra features, no drive-by refactoring.
4. **Verify GREEN** — run it, then run the project's suite (bare `cargo test` / `pytest` /
   `npm test`), not just your file. A green single file is not a green suite.
5. **REFACTOR** — clean up only while green; do not add behavior.

Reject the standard rationalizations: "too simple to test", "I'll test after", "already
manually tested", "TDD slows me down", "keep the code as reference".

## Rulings, not stalls

Conflicts, ambiguities, and plan defects: decide them. The **spec is the binding
authority**, the plan is its argument, and your judgment settles what neither answers.
Record every decision as:

```
Ruling: <what you decided> — <why> — <what it costs if wrong>
```

Then continue. Never deviate silently — an unledgered deviation is a decision made in
secret.

## Four stop conditions (and only these)

Stop and ask only for:

1. an irreversible or destructive operation;
2. a security-sensitive action;
3. a side effect outside the workspace (merge, push to a shared branch, publish);
4. a plan so broken that every path forward is a guess.

Otherwise do not ask "should I continue?" — proceed.

- **Manual mode:** the pause after every task (per `/implement`) is the checkpoint. Do not
  add extra permission questions on top of it.
- **Machine mode:** continuous execution. Do not pause between tasks; the fresh reviewer
  per task is the checkpoint. Only the four conditions above interrupt the run.

The ledger exists so a later session can resume either way.

## Completion contract (per task)

Before writing a task's ledger line, all of these must be true **with evidence from this
session** — not inferred from the diff looking right:

- every test the task names exists and ran; you read the output;
- the final test run passed — command and result recorded in the ledger line;
- every `Expected:` value was compared against real output;
- every deviation has a `Ruling:` line.

`references/integrity.md` → Verification before completion governs the claim. If any item
is missing, the task is not complete — finish it.

## When verification fails

Find the root cause before changing code. Never patch the symptom to make the expected
output match. Reproduce with a failing test first whenever possible: the test proves the fix
and prevents regression. "Fixed" without a test that failed first is a diff and a hope.

## Machine review loop (machine mode)

Per task, after the completion contract is satisfied:

1. Record `BASE` **before** starting the task (`git rev-parse HEAD`).
2. Build a review package into a file, not into context:
   `git log --oneline BASE..HEAD`, `git diff --stat`, `git diff -U10`.
3. Dispatch a **fresh** reviewer subagent — never the implementer — using
   `references/reviewer.md` → Task review. Give it file paths (brief, package) and the
   binding global constraints copied verbatim. Never paste the diff into the controller
   context, and never tell the reviewer what not to flag.
4. Both verdicts must be clean: **spec compliance PASS** and **quality APPROVED**.
5. Route findings:
   - **Minor** → ledger `minor (deferred)`, never enter the loop.
   - **Critical / Important** → fix loop. Rounds 1–3: resume the same implementer. Rounds
     4–5: a fresh implementer on a stronger model. Each round ends with a scoped re-review
     of the fix diff only (`references/reviewer.md` → Re-review).
   - **Cannot verify from diff** items: resolve them yourself before completing the task —
     you hold the cross-task context the reviewer lacks. A confirmed gap is a failed spec
     review and enters the loop.
6. **Cap: 5 rounds.** At the cap, adjudicate every open finding yourself and park it with a
   ledger `Ruling:` line. Silent discards are forbidden.
7. Never fix findings in the controller session — your context stays clean for coordination,
   and unreviewed fixes skip the gate.

Batch same-shape work (a repeated one-line edit across files) into one dispatch and one
review, instead of one cycle per file.

## Final review (convergence)

Verdict definitions: `references/integrity.md` → Convergence.

- Review the **whole diff** against spec / design / tasks — not just "tests pass".
- **Machine mode:** dispatch a fresh reviewer on the most capable available model with the
  whole-branch package and `references/reviewer.md` → Final review. One fix pass for
  Critical/Important, one scoped re-review, then adjudicate residuals.
- **Manual mode:** run the same review as a separate pass over the whole-branch package.
- Map every `FR-*`, `SC-*`, and task to evidence; report `converged` / `partial` /
  `failed`, with gaps listed.
- Findings get exactly one fix pass; each fix is verified by a test that failed first,
  then the full suite.
- Deferred minor findings are listed explicitly — never silently dropped.
- Requirements coverage is checked separately from tests passing; a green suite does not
  prove the spec was met.
