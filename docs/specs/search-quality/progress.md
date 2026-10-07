# Search-quality — progress ledger

Plan: `docs/specs/search-quality/tasks.md` · Spec: `docs/specs/search-quality/requirements.md`
Machine mode: fresh reviewer subagent per task; ledger appended with each task commit.

Task SQ1: complete (commits bd4e707..9ba793c, tests: `cargo test --locked --test search_quality` → ok (1 passed, 1 ignored); full suite 315 passed / 0 failed / 7 ignored)
Task SQ1: minor (deferred): brief-named `recall_at_k` helper not implemented — recall представлен `hit`/`hit_rate` (single-relevant кейсы); переименование при SQ2.
Task SQ1: minor (deferred): `ndcg_at_10` фактически считает nDCG@k (k=3) — переименовать при SQ2.
Task SQ1: minor (deferred): baseline `hit` не различает expected-missing и absent-matched; SQ2-gate добавит положительную/отрицательную половины SC-3.
Task SQ1: minor (deferred): baseline.json без trailing newline.
Task SQ2: complete (commits 2d57d86..ebc3f62, tests: `cargo test --locked --test search_quality` → ok (3 passed, 1 ignored); full suite 317 passed / 0 failed / 7 ignored)
Task SQ2: minor (deferred): brief-named `assert_no_regression` заменён на `regression_failures` + `no_regression_gate` (ревьюер подтвердил допустимый дрейф).
Task SQ2: minor (deferred): tolerance читается из `baseline.json` — осознанно (baseline = отчёт с порогами, M3); правка фикстуры проходит через ревью коммита.
Task SQ2: minor (deferred): отдельный `recall_at_k` helper не заведён — recall выражен `hit`/`hit_rate` (single-relevant кейсы).
