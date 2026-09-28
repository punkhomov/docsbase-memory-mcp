---
description: Generate or update requirements.md (SPECIFICATION) for a feature
---

Load the `project-design` skill and follow `references/documents.md` and `references/integrity.md`.

Generate or update `docs/specs/<feature-slug>/requirements.md` for: $ARGUMENTS

Rules:
- If discovery is incomplete, run the Discovery phase first (see `/requirements`).
- Include every required section from `references/documents.md`; no generic filler.
- Prioritized user stories P1… must each be independently testable (Independent test field).
- Number requirements `FR-*` / `NFR-*`; make acceptance criteria observable and testable;
  add measurable success criteria `SC-*`.
- For an existing spec, mark changes as `ADDED` / `MODIFIED` / `REMOVED` instead of
  silently rewriting.
- Never guess silently: unresolved points are marked `[NEEDS CLARIFICATION: ...]`.
- Run the red-team vectors and consistency audit; record challenges and resolutions in
  `Assumptions & challenges`, and show findings before requesting approval.
- Run the silent integrity check and list assumptions, contradictions, and open questions.
- Scale depth to project size.
- Pause after writing and wait for approval before starting `design.md`.
- Do not write implementation code.
