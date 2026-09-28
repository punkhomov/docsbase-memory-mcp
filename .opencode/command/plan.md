---
description: Generate design.md (IMPLEMENTATION) and tasks.md for a feature
---

Load the `project-design` skill and follow `references/documents.md` and `references/integrity.md`. Route language-specific design questions to the existing global skills.

Generate `docs/specs/<feature-slug>/design.md` then `docs/specs/<feature-slug>/tasks.md` for: $ARGUMENTS

Rules:
- Read `requirements.md` first; every design element must trace to an `FR-*` / `NFR-*`.
- Present 2–3 options with trade-offs for significant decisions; recommend one.
- Include Required/Forbidden stack and Global constraints with exact values.
- tasks.md: plan header (Goal, Architecture, Spec path, Global constraints, Review focus);
  each task has Files (Create/Modify/Test), Interfaces (Consumes/Produces), TDD steps with
  exact commands and expected output, acceptance criteria, and a verify command.
- Run the task-list self-review: spec coverage, type consistency, review focus, proportion.
- Run the red-team vectors and consistency audit before presenting each document.
- Run the silent integrity check; record conflicts as short ADRs in `design.md`.
- Pause after `design.md` for approval before writing `tasks.md`.
- Do not implement code during this command.
