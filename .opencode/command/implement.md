---
description: Execute spec tasks in manual (stop-for-review) or machine (reviewer subagent) mode
---

Load the `project-design` skill and run the **EXECUTION** phase, following
`references/execution.md`.

Mode: `manual` (default) or `machine`. Accept `--manual` / `--machine` in the arguments.
If the mode is not specified, ask with the `question` tool before starting.

Target: $ARGUMENTS (ignoring any mode flag)

Rules:
- Before starting, read `requirements.md`, `design.md`, and `tasks.md`.
- Implement exactly one task — the next pending one in dependency order.
- TDD: write the failing test first, watch it fail, then the minimal code. Run the project's
  suite, not just your file.
- Maintain the progress ledger (`docs/specs/<slug>/progress.md`): one line per task, and a
  `Ruling:` line for every deviation (`<what> — <why> — <cost if wrong>`).
- Stay within the task's declared files; if the design must change, stop and update the
  documents first, then ask for approval.
- Evidence before claims: run the task's verify command fresh and report the actual output
  (test counts, exit codes). Never claim success from memory or expectation.

Manual mode (default):
- STOP after each task: report what changed and the verification evidence, wait for review.
- Do not auto-advance.

Machine mode:
- After each task's completion contract, dispatch a fresh reviewer subagent using
  `references/reviewer.md` → Task review (pass file paths, never paste the diff).
- Both verdicts must be clean: spec compliance PASS and quality APPROVED.
- Findings: Minor → ledger; Critical/Important → fix loop (rounds 1–3 same implementer,
  4–5 fresh and stronger), scoped re-review each round, cap 5, then adjudicate with rulings.
- Do not pause between tasks; only the four stop conditions interrupt.
- Requires a subagent/Task tool; if unavailable, fall back to manual and say so.

After the final task, run CONVERGENCE (machine mode: dispatch
`references/reviewer.md` → Final review on the most capable model) and report
`converged` / `partial` / `failed` with evidence and gaps.
