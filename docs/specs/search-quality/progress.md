# Search-quality — progress ledger

Plan: `docs/specs/search-quality/tasks.md` · Spec: `docs/specs/search-quality/requirements.md`
Machine mode: fresh reviewer subagent per task; ledger appended with each task commit.

Task SQ1: complete (commits bd4e707..9ba793c, tests: `cargo test --locked --test search_quality` → ok (1 passed, 1 ignored); full suite 315 passed / 0 failed / 7 ignored)
Task SQ1: minor (deferred): brief-named `recall_at_k` helper not implemented — recall представлен `hit`/`hit_rate` (single-relevant кейсы); переименование при SQ2.
Task SQ1: minor (deferred): `ndcg_at_10` фактически считает nDCG@k (k=3) — переименовать при SQ2.
Task SQ1: minor (deferred): baseline `hit` не различает expected-missing и absent-matched; SQ2-gate добавит положительную/отрицательную половины SC-3.
Task SQ1: minor (deferred): baseline.json без trailing newline.
