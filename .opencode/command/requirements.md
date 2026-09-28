---
description: Start or continue requirements gathering for a feature or project
---

Load the `project-design` skill and follow `references/elicitation.md`.

Run the **Discovery** phase for: $ARGUMENTS

Rules:
- Classify the request first (spike / bounded / architectural) and say the classification
  out loud. Decompose multi-subsystem ideas into separate specs before refining details.
- Extract before asking; do not re-ask what is already known.
- Tap, don't type: use the `question` tool for 2–4 option decisions.
- Batch at most 1–3 related questions per turn.
- Ask binary decisions one at a time, each with a smart default and a one-line reason.
- Challenge as you go: for each requirement ask at least one adversarial question, flag
  weasel words, and surface contradictions in the same turn (Adversarial pass).
- Never guess silently: mark unresolved points `[NEEDS CLARIFICATION: ...]`.
- Stop when you have enough, summarize the decisions, and wait for confirmation.
- Do not write any code and do not generate `requirements.md` until the summary is confirmed.
