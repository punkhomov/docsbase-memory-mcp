---
name: project-design
description: "Use when planning a project or feature before code is written, or when verifying a result against its spec. Keywords: plan a project, spec this out, write a spec, requirements, requirements gathering, elicitation, architect a system, design a system, implementation plan, break into tasks, project blueprint, acceptance criteria, trade-offs, ADR, risk register, ready to implement, verify against spec, converge. Russian: спроектировать, требования, собрать требования, ТЗ, архитектура, план реализации, разбить на задачи, проектирование."
license: MIT
metadata:
  version: "1.3.0"
  category: development
  derived-from: "ersinkoc/project-architect (MIT), rizethereum/claude-code-requirements-builder (MIT), ncklrs/copilot-agent-prompts (MIT), ashishkujoy/ai-coding-agent (MIT), github/spec-kit (MIT), Fission-AI/OpenSpec (MIT), obra/superpowers (MIT); BA-Kit concepts (CC BY-NC 4.0, ideas only)"
---

# Project Design

Turn an idea into an **implementation-ready blueprint before any code is written**, then
**verify the result against that blueprint**. The value is in *thinking first*: explicit
requirements, surfaced assumptions, named trade-offs, and ordered tasks.

## Scale the process first

Classify the request before doing anything and say the classification out loud, so it can
be overridden.

| Path | Signal | Process |
|---|---|---|
| **Don't engage** | typo, one-line bugfix, throwaway script | just do it; no ceremony |
| **Spike** | feasibility question; output is an answer, not code you keep | 2–3 sentence probe plan → nod → investigate cheaply → report |
| **Bounded** | small change to a flow that already exists in this repo | clarifying questions → short design in chat → explicit yes → implement |
| **Architectural** | new project/subsystem, or changes that restructure interfaces | full pipeline below |

When in doubt, take the heavier path. Hidden complexity is a **one-way ratchet**: stop,
say so, upgrade the path. A spike's output is an answer; keeping its code is a new request.

## Core principles

1. **Extract before asking.** Harvest everything already stated (chat, docs, code, the
   linked design doc). Never re-ask an answered question.
2. **Tap, don't type.** Any question with 2–4 crisp options uses the `question` tool.
   Open-ended freeform is reserved for intent/purpose questions.
3. **Batch 1–3 related questions per turn, never 5+.** Intent/purpose questions are asked
   one at a time; option choices may be batched.
4. **Smart defaults + rationale.** Every question carries a sensible default and a one-line
   reason. "idk" / "you pick" means take the default and state it explicitly.
5. **Adaptive depth.** Prototype = 5–8 questions. Full product = 15–25. Stop when you can
   write a useful specification.
6. **Blueprint before code.** No implementation until the spec and plan are approved.
7. **Recommend, don't dictate.** Present 2–3 options with trade-offs; if asked to choose,
   choose and explain why.
8. **Traceability.** Every design element traces to a requirement; every task references
   its design element. No orphan decisions, no orphan tasks.
9. **No filler.** Every line must be specific to THIS project. Delete generic boilerplate.
10. **Challenge, don't accept.** Every requirement and design decision gets at least one
    adversarial question before acceptance. Nitpick contradictions in the same turn.
11. **Verify, don't assert.** Claims about paths, APIs, "there is no X" need a tool check.
12. **Evidence before claims.** No success/completion claim without a fresh verification
    command and its actual output. See `references/integrity.md`.

## Workflow (gated)

Before Discovery, check for `docs/specs/constitution.md` — short project principles
(quality, testing, dependency, naming rules) that govern every spec. If none exist and the
project is non-trivial, offer to create one.

```
Discovery → SPECIFICATION → IMPLEMENTATION → TASKS → EXECUTION → CONVERGENCE
              what             how             work      build        verify
```

Run phases **sequentially**. **Pause for approval after each document.** **Approval is
stage-scoped:** a yes to the summary approves only the next artifact, never permission to
skip later gates.

**Every gate runs a challenge pass** (challenger + consistency) before approval — see
`references/integrity.md`:

- **Discovery:** challenge each requirement as it is captured and surface contradictions in
  the same turn — see `references/elicitation.md` → Adversarial pass.
- **SPEC / IMPLEMENTATION / TASKS:** run the red-team vectors and the cross-artifact
  consistency audit, show findings, then request approval.

1. **Discovery** — read `references/elicitation.md`. Classify scope; if the idea spans
   multiple independent subsystems, split into separate specs first. Understand identity,
   scope, stack, data, features, constraints. Challenge, summarize, confirm.
2. **SPECIFICATION** — what the thing is, with prioritized user stories and measurable
   success criteria. Read `references/documents.md`.
3. **IMPLEMENTATION** — how it is built: architecture, components, interfaces, data flow,
   dependencies with rationale, error handling, testing strategy, risks and trade-offs,
   Required/Forbidden stack constraints. Short code sketches for critical patterns.
4. **TASKS** — ordered, dependency-aware work items. Each task: single session, exact files
   (create/modify/test), interfaces consumed/produced, acceptance criteria, verify command.
5. **EXECUTION** — one task at a time, following `references/execution.md`: TDD loop,
   progress ledger, rulings instead of stalls, completion contract. Two modes:
   `manual` (default) stops after each task for human review; `machine` is continuous with
   a fresh reviewer subagent per task (`references/reviewer.md`). Read spec + design + tasks
   first, run the verify command, report actual output.
6. **CONVERGENCE** — after implementation, verify the code against spec / design / tasks
   (whole-diff review, not just tests) and report `converged` / `partial` / `failed`, with
   evidence and gaps listed.

Phases produce artifacts the next phase can read **cold** — assume no shared conversation.
Each artifact must be self-contained.

## Output location

Write artifacts to `docs/specs/<feature-slug>/` in the current project:

```
docs/specs/<feature-slug>/
├── requirements.md   # SPECIFICATION
├── design.md         # IMPLEMENTATION
└── tasks.md          # TASKS
```

Updating an existing spec uses **delta markers** (`ADDED` / `MODIFIED` / `REMOVED`) instead
of silently rewriting — see `references/documents.md`. Never overwrite silently; show a
diff or ask first.

## Required sections per document

- **SPECIFICATION**: Problem · Goals / Non-goals · Users / Actors · Prioritized user
  stories (P1…, each independently testable, with Given/When/Then acceptance scenarios) ·
  Functional requirements · Non-functional requirements (ISO 25010) · Data model ·
  Interfaces · Measurable success criteria · Assumptions & challenges · Out of scope ·
  Open questions
- **IMPLEMENTATION**: Overview · Global constraints · Architecture (Mermaid when useful) ·
  Components · Module interfaces · Data flow · Dependencies + rationale ·
  Required/Forbidden stack · Error handling · Testing strategy · Risks & trade-offs ·
  Alternatives considered (ADR)
- **TASKS**: plan header (Goal · Architecture · Spec path · Global constraints ·
  Review focus) + numbered tasks with files, interfaces, TDD steps, acceptance criteria,
  verify command, dependency order

## Language & domain rules

- Write specs in the language of the user's docs/team; keep identifiers and APIs verbatim.
- Route language-specific depth to the existing skills (Rust: `m01`–`m15`, `domain-*`,
  `unsafe-checker`, `coding-guidelines`; versions via `rust-learner`) rather than guessing.
- Prefer quality attributes from ISO 25010 when stating non-functional requirements:
  performance, scalability, reliability, security, maintainability, portability.

## References

| File | Read when |
|------|-----------|
| `references/elicitation.md` | Before asking any discovery questions |
| `references/documents.md` | Before generating SPECIFICATION / IMPLEMENTATION / TASKS |
| `references/integrity.md` | Before final claims, at every gate, and on completion |
| `references/execution.md` | During EXECUTION and CONVERGENCE (TDD, ledger, rulings) |
| `references/reviewer.md` | Machine mode: prompts for task review, re-review, final review |
