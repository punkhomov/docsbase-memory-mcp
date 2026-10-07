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
Task SQ3: complete (commits 43b4724..445b58b, tests: `cargo test --locked --test chunker --test tantivy_index` → 20/14 ok; full suite 320 passed / 0 failed / 7 ignored)
Task SQ3: Ruling: `CHUNK_OVERLAP` (=150) определён в SQ3 (chunk.rs), хотя splitter — SQ4: порог penalty из брифа требует константу — если неверно, константа переедет в SQ4 без изменения поведения.
Task SQ3: minor (deferred): `LONG_CHUNK_PENALTY` doc не упоминает `+CHUNK_OVERLAP` (src/index/tantivy_index.rs:21).
Task SQ3: minor (deferred): `CHUNK_OVERLAP` doc не упоминает SQ3-потребителя (penalty) (src/index/chunk.rs:23).
Task SQ3: minor (deferred): тестовые границы 1590/1690 дублируют 1500+150 вместо вывода из констант (tests/tantivy_index.rs:330).
