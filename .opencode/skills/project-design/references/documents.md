# Documents

Templates and rules for the planning artifacts. Every document must be specific to THIS
project — delete any section that would be generic boilerplate. Write so the next phase can
read the artifact **cold**: it must not depend on the conversation that produced it.

## requirements.md — SPECIFICATION (the *what*)

Technology-aware but implementation-agnostic. No code, no implementation details.

Required sections:

1. **Problem** — what hurts today, for whom, why now.
2. **Goals / Non-goals** — explicit non-goals prevent scope creep.
3. **Users / Actors** — who uses it and in what context.
4. **Prioritized user stories** — `P1`, `P2`, … ordered by importance. Each story must be
   **independently testable and valuable on its own** (a vertical slice), and carries:
   - format: `As a <role>, I want <capability>, so that <benefit>.`
   - **Why this priority** — one line.
   - **Independent test** — how it is verified alone.
   - **Acceptance scenarios** — Given/When/Then lines (EARS where natural:
     *"When <trigger>, the system shall <response>."*)
5. **Functional requirements** — numbered `FR-1`, `FR-2`, … each testable, marked
   must / should / may.
6. **Non-functional requirements** — numbered `NFR-1`, … using ISO 25010 attributes
   (performance, scalability, reliability, security, maintainability, portability) with
   measurable targets (latency, memory, corpus size).
7. **Data model** — entities, fields, relationships, lifecycle.
8. **Interfaces** — CLI commands, MCP tools, HTTP endpoints, file formats. Signatures and
   shapes, not implementations.
9. **Measurable success criteria** — numbered `SC-1`, … technology-agnostic and objectively
   checkable.
10. **Assumptions & challenges** — every challenged requirement, the adversarial question
    asked, and how it was resolved (accepted / changed / dropped). No silent assumptions.
11. **Out of scope** — explicitly deferred.
12. **Open questions** — anything unresolved, with a proposed default.

Rules:
- Every requirement must pass the adversarial pass (see `references/elicitation.md`) before
  it is written here. Unchallenged requirements are a defect.
- Never guess silently. Unresolved points are marked inline as
  `[NEEDS CLARIFICATION: <question>]` and repeated under Open questions.
- **Updating an existing spec:** mark changes as delta, don't rewrite history:
  `ADDED` / `MODIFIED` / `REMOVED` above each affected requirement or story, with the new
  Given/When/Then scenarios. Removed requirements state why.

## design.md — IMPLEMENTATION (the *how*)

Translates the spec into an implementable design. Includes short code sketches for
critical patterns (signatures, types, structural examples — not full implementations).

Required sections:

1. **Overview** — one paragraph linking back to the spec.
2. **Global constraints** — version floors, dependency limits, naming/copy rules, platform
   requirements. One line each, exact values.
3. **Architecture** — component map; Mermaid diagram when it clarifies.
4. **Components** — each with responsibility, public interface, and dependencies.
5. **Module interfaces** — function/type signatures, key invariants.
6. **Data flow** — how data moves end to end; storage and lifecycle.
7. **Dependencies + rationale** — every external crate/library and why it was chosen.
8. **Required / Forbidden stack** — what must be used and what must not (e.g.
   `Required: tokio`; `Forbidden: any embedding API`). Prevents drift during execution.
9. **Error handling** — error taxonomy, propagation, user-visible behavior.
10. **Testing strategy** — unit / integration / golden tests, fixtures, what is not tested.
11. **Risks & trade-offs** — what could go wrong, what was traded away.
12. **Alternatives considered** — ADR-style: option, why rejected.

Rules:
- Recommend 2–3 options with trade-offs for significant decisions; never dictate.
- Every design element must reference the `FR-*` / `NFR-*` / `SC-*` it satisfies.
- Design units with clear boundaries and one responsibility. Files that change together
  live together; split by responsibility, not by layer.
- Include a directory structure with a one-line purpose per file.

## tasks.md — TASKS (the *work*)

Ordered, dependency-aware work items written for an implementer with **zero context**.

Plan header (required):

```markdown
# <Feature> Implementation Plan
**Goal:** one sentence.
**Architecture:** 2–3 sentences.
**Spec:** docs/specs/<slug>/requirements.md
**Global constraints:** copied verbatim from design.md, one line each.
**Review focus:** the 3–5 failure modes most likely to bite a real user, each pinned to a
test in the owning task.
```

Task structure:

- **ID** — `T1`, `T2`, … in dependency order.
- **Goal** — one sentence.
- **Files** — exact paths: `Create:` / `Modify:` (with line ranges when known) / `Test:`.
- **Interfaces** — `Consumes:` and `Produces:` exact names, parameters, return types, so
  neighbouring tasks can be implemented independently.
- **Steps** — one action with a checkable result each:
  1. write the failing test (test name + assertions as code);
  2. run it and confirm it fails (exact command, expected failure);
  3. implement the minimal code (exact signature; body only when the algorithm is not
     determined by the signature and tests);
  4. run the test and confirm it passes;
  5. commit with the task's change.
- **Acceptance** — ties to `FR-*` / `NFR-*`; observable.
- **Verify** — the command that proves it and the output that means success.

Rules:
- **Right-sizing:** a task is the smallest unit with its own test cycle that a reviewer
  could meaningfully accept or reject. Fold scaffolding/config/docs into the task whose
  deliverable needs them. If a task contains "and" between two user behaviors, split it.
- Each task is completable in a **single agent session** and self-contained.
- Strict dependency order: no task assumes work that appears later.
- A line that decides nothing (`TBD`, "handle edge cases", "add validation") is a defect;
  so is transcribing the whole implementation. A plan is the set of decisions the
  implementer cannot make alone.
- Scale: weekend project ≈ 15–30 tasks; larger system ≈ 100+. Match depth to ambition.

Task-list self-review before showing the plan:
1. **Spec coverage** — every requirement maps to a task; list gaps.
2. **Type consistency** — names/signatures used in later tasks match earlier definitions.
3. **Review focus** — the 3–5 failure modes most likely to bite users, each pinned to a
   test in the owning task.
4. **Proportion** — a plan several times longer than the spec is a transcript, not a plan.

## constitution.md — project principles (optional, once per project)

Path: `docs/specs/constitution.md`. A short, durable document that governs every spec: code
quality bar, testing expectations, dependency policy, naming and copy rules, platform and
version floors. Written once when the project starts; specs reference it instead of
repeating the rules. Update it deliberately — it is the project's standing law, not a
per-feature choice.

Example lines: `Tests: every FR has at least one automated test.` ·
`Dependencies: one new crate per task maximum, with justification.` ·
`Errors: no unwrap in library code.`

## Cross-document rules

- Documents are sequential: requirements → design → tasks. Never skip ahead.
- Pause for approval after each document before writing the next.
- Before each approval, run the red-team vectors and the consistency audit
  (`references/integrity.md`) and include the findings in the document or the message.
- Cross-reference: design cites requirements; tasks cite design and requirements.
- No orphan decisions and no orphan tasks.
- When requirements change after approval, update all three and mark the diff.

## Handling partial input

| Scenario | Action |
|---|---|
| Vague one-liner | Full elicitation flow |
| Detailed brief | Extract answers, ask only gaps |
| Existing spec | Validate against the checklist, continue from design |
| "Just the spec" | Generate `requirements.md`, offer to continue |
| "Skip to tasks" | Lightweight requirements + design, then detailed tasks |
| Multi-subsystem idea | Decompose into sub-specs first (see `references/elicitation.md`) |
