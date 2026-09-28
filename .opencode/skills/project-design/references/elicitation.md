# Elicitation

Structured discovery before any document is generated. The goal is **sufficient**
understanding, not exhaustive interrogation.

## Core strategy

1. **Extract before asking.** Scan the conversation, linked docs, the repo, and any
   provided brief. Convert already-answered points into stated assumptions.
2. **Tap, don't type.** Use the `question` tool for any decision with 2–4 clear options.
3. **Batch 1–3 related questions** per turn. Never 5+.
4. **Adapt depth to ambition.** Prototype → 5–8 questions. Full product → 15–25.
5. **Stop when you have enough.** A useful spec beats an interrogation.
6. **Check scope before depth.** A request spanning multiple independent subsystems gets
   decomposed first — refining details of the wrong-sized project wastes the whole pass.

## Question tiers

- **Tier 1 — Blockers** (always ask if unanswered): project identity, type, scope,
  language/stack direction.
- **Tier 2 — Important** (medium+ projects): data & storage, features, auth, UI,
  deployment, dependencies posture.
- **Tier 3 — Depth** (large projects): scale, real-time, API style, open source,
  team size, deadlines, explicit non-goals.

## Two-phase question flow

Borrowed from `rizethereum/claude-code-requirements-builder`: ask high-level product
questions first, then analyze the codebase autonomously, then ask expert questions with
real file paths.

1. **Discovery (product-level, yes/no + smart defaults).** 3–5 questions about the
   problem space. One at a time. "idk" → take the default and say which.
2. **Context gathering (autonomous).** Search/read the repo for similar features,
   patterns, constraints, integration points. Do not ask the user what the code can tell
   you.
3. **Detail (expert-level).** 3–5 sharp yes/no questions referencing actual paths and
   components, each with a default grounded in what you found.

## Scope decomposition

Before refining details, check the idea's size. If it describes multiple independent
subsystems (e.g. "a service with search, sync, an API, and a UI"), stop and decompose:

1. Name the independent pieces.
2. State how they relate and in what order they should be built.
3. Give each piece its own spec → design → tasks cycle.
4. Brainstorm only the **first** sub-project through the normal flow.

Each sub-project must produce working, testable value on its own. Refining details of a
wrong-sized project is the most expensive mistake in this whole process.

If the user is unsure what to build at all, offer a short **explore** pass first: map the
problem space, read the relevant code, weigh 2–3 directions — no artifacts, no gates.
Explore ends with a recommendation and a proposed scope; then normal discovery begins.

## Question format

For binary decisions:

```
Q: <specific yes/no question>?
   Default if unknown: <YES/NO> — <one-line reason>.
```

For choices, use the `question` tool:

```
Question: "Where will this run?"
Options: ["Local only (CLI/desktop)", "Self-hosted server", "Cloud / SaaS", "Hybrid"]
```

Rules: one binary question at a time; batch only choice questions. Never ask open-ended
questions when a concrete option set exists.

## Requirement aspect checklist

From `ashishkujoy/ai-coding-agent` — break any request into these aspects and fill gaps:

- language / stack preference
- input format & source (user input, file, API, DB)
- expected output (return value, file, UI, report, stream)
- processing logic / operations
- constraints: performance, size, complexity, edge cases
- environment / context (production, learning, internal tool)
- concrete example inputs and outputs
- preferences: libraries, style, version compatibility

## Adversarial pass (challenger + consistency, inline)

Never accept a requirement at face value, and never defer a contradiction. Challenge as
requirements are captured, in the same turn — this is part of discovery, not a later phase.

For each significant requirement, ask **at least one** challenge, choosing the
highest-leverage one:

- *What breaks if we don't build this?* — exposes requirements that are nice-to-have.
- *Who loses or disagrees if we do?* — exposes stakeholder conflicts.
- *What is the failure mode?* — exposes unhandled edge cases early.
- *How will we measure success?* — exposes untestable requirements.
- *What is the simplest version that still solves the problem?* — exposes gold-plating.

Consistency checks, run continuously:

- **Weasel words:** `fast`, `secure`, `simple`, `scalable`, `better`, `modern` — demand a
  measurable definition or mark it as an open question.
- **Ambiguity:** if two readings exist, state both and ask which is meant.
- **Contradiction:** if a new requirement conflicts with a captured one, name both sides
  immediately and ask which wins. Do not carry both forward silently.
- **Hidden assumptions:** list every assumption the requirement relies on and confirm it.

Keep the pressure **targeted, not exhausting**: 1–2 challenges per requirement, batched
where possible, and use `question` options (`"A", "B", "both", "drop it"`) instead of
freeform. During discovery, challenge questions count toward the same 1–3-per-turn budget.

**Requirement acceptance bar** — a requirement is only "captured" when it has:

| Field | Meaning |
|---|---|
| Owner | who wants it / who is accountable |
| Trigger | when it applies (event, state, request) |
| Observable outcome | what an observer can verify |
| Failure mode | what happens when it goes wrong |
| Non-goal | what it deliberately does not cover |

Anything missing goes to the challenge list, not silently into the spec.

## Adaptive matrix

| User signal | Ask | Depth |
|---|---|---|
| One-line "I want to build X" | Tiers 1 fully, Tier 2 selectively | High |
| Detailed 3+ paragraph brief | Gaps in Tier 1 only | Low |
| "Help me with everything" | All tiers | Maximum |
| "Just a quick spec" | Tier 1, sensible defaults elsewhere | Minimal |
| Existing spec provided | Extract answers, ask only gaps | Varies |
| "You decide" | Pick the safe/common option, state it | N/A |

## Before generating

Summarize and confirm:

```
Here is what I understand:
- Project: ...
- Type: ...
- Stack: ...
- Core features: ...
- Constraints: ...
- Out of scope: ...
- Assumptions: ...

Does this look right? I will start with requirements.md.
```

Wait for confirmation. If the user corrects anything, update the summary and re-confirm.

Unresolved points are never guessed silently. Mark them inline in the document as
`[NEEDS CLARIFICATION: <question>]` and also list them under Open questions. A default is
stated as a default, not presented as a decision the user made.

## Drift recovery

If the conversation drifts into implementation, open questions, or premature detail:

1. Stop.
2. State which phase rule was violated.
3. Return to the last confirmed artifact and resume the workflow.

This is the `/remind` behavior from `rizethereum` — self-enforced, no user request needed.
