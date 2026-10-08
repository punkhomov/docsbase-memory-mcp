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
Task SQ4: complete (commits 525931a..a777e85, tests: `cargo test --locked` → 324 passed / 0 failed / 7 ignored; chunker 24 tests)
Task SQ4: Ruling: рекурсивная сегментация — только для секций > cap (иначе ломаются golden-ранги продукта); fence/table атомарны; heading-caption merge; overlap = min(CHUNK_OVERLAP, cap/10), пропускается для merged/atomic — иначе bound cap+overlap не держится.
Task SQ4: Ruling: chunker-тесты, фиксировавшие v1-семантику (oversized paragraph intact, fence одним чанком, trailing-prose prose-kind, kind cap 12), обновлены под FR-4; fence/table-целостность сохранена и покрыта.
Task SQ4: minor (deferred): merge_preceding_heading требует точной смежности — дропнутый whitespace-only piece может оставить голый заголовок; `heading_merged: true` у atomic-кусков противоречит док-комментарию поля.
Task SQ4: minor (deferred): границы только ASCII-пунктуация (Unicode punct → char fallback); merge_preceding_heading не проверяет бюджет merged-размера в байтах-не-символах.
Task SQ4: minor (deferred): тест-гигиена — line coverage по первому токену, размер overlap не ограничен тестом.
Task SQ5: complete (commits 0868702..b222712, tests: `cargo test --locked` → 328 passed / 0 failed / 7 ignored)
Task SQ5: Ruling: файл версии пишется в create_fresh и в ветке нового каталога до создания индекса (рядом с REBUILD_MARKER); отсутствие/несоответствие = legacy → рекреэйт + полный reindex; ReadIndex → Error::Project с инструкцией.
Task SQ5: minor (deferred): ReadIndex::open при отсутствующем meta.json теперь падает на version-check с сообщением «pipeline unknown» вместо ошибки отсутствия индекса; мусорный контент файла не покрыт тестом.
Task SQ5: minor (deferred): write_tokenizer_version не имеет rollback'а remove_dir_all как write_rebuild_marker — безопасно только за счёт порядка (маркер всегда первым); закрепить комментарием/guard.
Task SQ5: minor (deferred): вариант с удалённым файлом версии проверяет только was_recreated, не полный reindex (покрыто родственным тестом index_job).
Task SQ6: complete (commits fb65d1e..b2d2443, tests: `cargo test --locked` → 330 passed / 0 failed / 7 ignored)
Task SQ6: Ruling: baseline.json регенерирован (санкционированный improvement): фраза "plan id" больше не матчит assessment_plan_id и находит prose-док — hit false→true, absent_hit true→false, ndcg 0.0→1.0; класс phrases 0.0→1.0. Механизм SQ2-gate допускает улучшения.
Task SQ6: Ruling: chunker-тест позиций positions_are_contiguous заменён на positions_are_one_per_segment (FR-7 меняет семантику позиций).
Task SQ6: minor (deferred): док-комментарий «positions are contiguous from zero» устарел; лимит длины тестируется только на raw-пути (camel/alnum части >40 — только через централизацию emit); 64-символьный blob-кейс дублирует 41; gap от дропнутого сегмента (blob) и пунктуационные сегменты без advance позиции не покрыты тестом.
Task SQ7: complete (commits e2c9579..1a965ca, tests: `cargo test --locked` → 334 passed / 0 failed / 7 ignored)
Task SQ7: Ruling: стем-вариант заменяет поверхностную форму standalone-слова (probe: PhraseQuery same-offset = конъюнкция, 0 хитов) — иначе кросс-форменный матч FR-9 невозможен; FR-9/ADR-S2/риск-таблицы аменднуты в requirements.md/design.md.
Task SQ7: Ruling: Snowball RV делит иллюстративное трио: замена/замены→зам, заменой→замен (офиц. vocab: алена→ал, времена→врем); unit-тест проверяет реальные классы; golden RU 5/5, класс ru 0.4→1.0; baseline регенерирован.
Task SQ7: Ruling: снапшоты цитат — delta (semantic_top3 3-я цитата → session-management.md, другой релевантный док; SC-1..SC-4 ассерты неизменны); тесты index_job (маркер quickly) и stored_text_len (терм refresh) обновлены под стемы.
Task SQ7: minor (deferred): offset стема указывает на поверхностный спан (потребителей нет, но инвариант не зафиксирован); длинные alnum-раны стеммятся до лимита MAX_TOKEN_CHARS; wildcard `_ => en` в analyzer_for — SQ10 обязан добавить явную ветку Arabic; нет теста «стем на позиции сегмента»/фразовой механики.
