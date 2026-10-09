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
Task SQ8: complete (commits 4161240..2619d5f, tests: `cargo test --locked` → 334 passed / 0 failed / 7 ignored)
Task SQ8: Ruling: golden-классы сатурированы (ru/en/identifiers/phrases 1.0/1.0; cjk/arabic ждут SQ10/11), sweep C1–C4 идентичен → константы не менялись (наименьшее структурное смещение); LONG_CHUNK_PENALTY на golden не упражняется (корпус < cap+overlap) — задокументировано в коде.
Task SQ8: Ruling: guard FR-11 — переиспользован SQ2 no_regression_gate (per-class hit_rate/ndcg + per-case hit/absent/ndcg); дубль не добавлялся (YAGNI).
Task SQ8: minor (fixed in-task): уточнены комментарии — penalty не упражняется golden; гейт сравнивает метрики, а не константы; regenerate_baseline не ограничивается гейтом.
Task SQ9: complete (commits 132bb6a..d8a2ed5, tests: `cargo test --locked` → 340 passed / 0 failed / 7 ignored)
Task SQ9: Ruling: normalize применяется к тексту до токенизации; фолды 1:1 и case-preserving (Ё→Е, İ→I) — иначе фолд снимал ALL-CAPS-защиту FR-10 (fix in-task по minor ревью); token-оффсеты для width-folded текста относительны нормализованного текста (потребителей оффсетов нет).
Task SQ9: Ruling: split_script_runs реализован уже в SQ9 (потребитель — SQ11); stem_language маршрутизируется через script_of (Latin→EN, Cyrillic→RU; café теперь стеммится).
Task SQ9: minor (deferred): edge-семантика split_script_runs (пунктуация/цифры/без букв → пустой Vec) не покрыта тестом; пробелы диапазонов скриптов (Latin Ext-C/D/E, CJK Ext B+, Bopomofo, Arabic 08A0/FB50/FE70) → Other; halfwidth katakana не фолдится во fullwidth; dotless ı не фолдится (турецкая пара асимметрична); мёртвый guard `if index > start`.
Task SQ10: complete (commits bddc357..80e914a, tests: `cargo test --locked` → 343 passed / 0 failed / 7 ignored)
Task SQ10: Ruling: baseline регенерирован — только arabic-кейс (hit false→true, ndcg 0→1; класс arabic 0.0→1.0) = целевой флип FR-13/SC-8; прочие классы без изменений.
Task SQ10: Ruling: analyzer_for получил явную ветку Arabic (закрыт SQ7-minor про `_ => en` wildcard); фолды Arabic — no-op для FR-10-гардов (регистр не трогается).
Task SQ10: minor (fixed in-task): прямые ассерты أإآ→ا/ة→ه/ى→ي и removal-путей; Arabic-входы в тесте идемпотентности; доки модулей дополнены FR-13.
Task SQ10: fixed bug: needs_fold range U+0640..=U+0652 захватывал арабские буквы U+0641–U+064A (clippy: unreachable 'ى') — сужен до harakat U+064B..=U+0652 + tatweel U+0640.
Task SQ11: complete (commits cb80055..6d6c250, tests: `cargo test --locked` → 349 passed / 0 failed / 7 ignored; все 6 классов 1.0/1.0)
Task SQ11: Ruling: биграммы — по всем символам CJK-рана (включая приклеенные цифры: 第1四半期 ≠ 第2四半期), без variant-dedupe (повторы несут позиции для фраз), одиночный символ → униграмма; не-CJK раны сохраняют одну позицию сегмента (SQ6-инвариант).
Task SQ11: Ruling: baseline регенерирован — только cjk-кейс (0.0→1.0) = целевой флип FR-12/SC-8; прочие классы без изменений; golden-снапшоты без дрейфа.
Task SQ11: minor (fixed in-task): комментарий cjk_segment уточнён (CJK-dominant, не pure); script_char возвращён в private (нужен только внутри textnorm).
Task SQ12: complete (commits ae1e95c..fb5a866, release: `cargo test --locked --release --test perf_budget` → 6 passed; debug: 349 passed / 0 failed / 8 ignored)
Task SQ12: Ruling: бюджеты не менялись; новые замеры: latin p95 564µs, cjk p95 4.88ms (50k чанков), cjk index 6.7MB/8.7MB (77%, sanity-ceiling 4×), full index 1000 файлов 154ms, 10k no-op 83ms, RSS delta 64MiB (бюджет 128), idle daemon 10MB; RSS-комментарий обновлён с ~32MiB на ~64MiB.
Task SQ12: Ruling: SC-7 — offline-гейт зелёный (Linux, 1 passed), `cargo tree -e normal` без сетевых крейтов (reqwest/ureq/curl/hyper/isahc/surf — пусто); debug-прогон игнорирует все перф-тесты.
Task SQ12: minor (fixed in-task): целочисленный ratio вместо float (clippy cast_precision_loss), assert index_bytes > 0, let-else в dir_size.
Task SQ12: minor (deferred): cjk p95-бюджет с запасом ~40× (не ловит регресс ×20, стиль существующих тестов); sanity-ceiling 4× не имеет spec-анкора; headroom намеренно большой.
Plan extended (post-review convergence): SQ13–SQ16 (tasks.md) — закрытие находок кросс-ревью SQ1–SQ12 (delta-протокол) и code-ревью (m15/m10); порядок: SQ13 (pin/penalty/инвариант) → SQ14 (контракт SQ1) → SQ15 (доки) → SQ16 (hygiene).
Task SQ13: complete (commits 84528e1..285240b, tests: chunker 26 / tantivy_index 20 / tokenizer 35; full suite 354 passed / 0 failed / 8 ignored)
Task SQ13: Ruling: oversized fence/table атомарны, > cap+overlap, без падений (проба 5018/4005 при 1650); SC-5-исключение фиксируется pin-тестами, delta-маркер — SQ15.
Task SQ13: Ruling: penalty достижим e2e — BM25-длина в токенах, блоб >40 симв. не индексируется, поэтому giant fence «короткий» в токенах и LONG_CHUNK_PENALTY — единственный тормоз (тест с симметричными fence-доками из реального chunker).
Task SQ13: Ruling: tie script_of 2:2 Latin/CJK → Latin (enum-порядок осознан); whole-токен не эмитится (stem-only путь), биграммы эмитятся — поведение запинено.
Task SQ13: minor (fixed in-task): конкретные line-span ассерты и целостность 400 строк таблицы; формулировка брифа про tie.
Task SQ13: minor (deferred): fallback в bound_prose — defense-in-depth, недостижим при контракте split_oversized_line (тестом не покрыт); untracked docs/specs/hybrid-providers/ — не относится к задаче.
Task SQ14: complete (commits 79024e0..e07dc0a, tests: search_quality 4 passed / 1 ignored; full suite 355 passed / 0 failed / 8 ignored)
Task SQ14: Ruling: контракт SQ1 выполнен буквально — recall_at_k (found-половина hit) и ndcg_at_10 (реальный truncate ≤10) используются в evaluate; кейсы k=3 < 10, поэтому baseline не регенерировался и метрики идентичны.
Task SQ14: Ruling: детерминизм доказан дважды — evaluate_is_deterministic (два независимых Bench) сравнивает полный сериализованный отчёт; непустота отчёта и наличие hit проверяются (не вакуумный).
Task SQ14: minor (fixed in-task): уточнён док recall_at_k (absent-случай), gate-кейс изолирует per-case nDCG-бранч (class_ndcg равны), обновлён док-комментарий теста, добавлена non-vacuity проверка.
Post-review SQ15: delta-маркеры: FR-3 + assumption #3 — дрейф хвостовых позиций citation-снапшотов (не только координаты chunk_id/lines), критерий hit@k per-case; SC-5 + design §9 — атомарные fence/table как единственное исключение, формулировки синхронизированы, устаревшая ссылка на «одиночный абзац» убрана; NFR-6 — sweep валиден только на дискриминирующем корпусе.
Post-review SQ15: учёт SQ4-delta (a777e85): снапшот error_message_top1 — хвостовая rank-3 замена cad/sheet-metal-rules.md (Diagnostics) → cad/thickness-glossary.md; per-case top-1 ассерт SC-2 держится; замена — следствие нового сплита (S2), покрыта delta-формулировкой FR-3.
Post-review SQ15: учёт SQ8-sweep (2619d5f), per-class hit_rate/ndcg (все достижимые классы сатурированы):
| Вариант | text | title | heading | identifiers | penalty | ru/en/identifiers/phrases |
|---|---|---|---|---|---|---|
| C1 current | 1.0 | 2.0 | 1.5 | 2.5 | 0.5 | 1.0/1.0 каждый |
| C2 | 1.0 | 3.0 | 2.5 | 2.5 | 0.5 | 1.0/1.0 каждый |
| C3 | 1.0 | 2.0 | 1.5 | 3.5 | 0.5 | 1.0/1.0 каждый |
| C4 | 1.0 | 2.0 | 1.5 | 2.5 | 0.25 | 1.0/1.0 каждый |
Варианты неразличимы на golden (cjk/arabic тогда ждали SQ10/11) → оставлены консервативные значения C1; LONG_CHUNK_PENALTY теперь достижим e2e (SQ13 pin).
Post-review SQ15: fixed/deferred: SQ13 minors — fixed (конкретные span-ассерты, формулировка брифа), deferred (fallback defense-in-depth); SQ14 minors — fixed (док recall_at_k, изоляция per-case nDCG-бранча, док-комментарий, non-vacuity); untracked docs/specs/hybrid-providers/ — вне задачи.
Task SQ15: complete (commits 359a64d..a47c805 + fix round, docs-only: FR-3/assumption #3/SC-5/NFR-6 delta-маркеры, design §9 + §5, ledger)
Task SQ15: Review: spec PASS, quality CHANGES (1 Important: §5/S2 безусловная cap-гарантия) → fix round (проза-квалификатор в §5/S2/SQ4-интерфейсе) → re-review ADDRESSED; остальные unqualified-упоминания закрыты в том же проходе.
Task SQ16: complete (commits 5d9c44b..f769cc5, tests: full suite 356 passed / 0 failed / 8 ignored; release perf_budget 6/6)
Task SQ16: Ruling: tokenize разбит на Emitter (whole/runs), тело ~30 строк (m15); camel_split — один расчёт на ран (single_camel переиспользуется при runs.len()==1 && script_runs.len()==1); #[must_use] на 6 хелперах; TextAnalyzer не имеет Default для составного стеммера — ручной impl Default остаётся.
Task SQ16: Ruling: merge_heading_only — push/pop без remove(index) в цикле (remove(len-2) сдвигает один элемент); первая версия с pop() удаляла новый piece (потеря текста) — поймана probe-тестом и исправлена; путь достижим только в oversize-секциях (под-cap возвращаются раньше), покрыт oversized_section_keeps_heading_in_first_piece (не-вакуумность: падает на багованной версии, проходит на BASE — parity).
Task SQ16: Ruling: перф до/после (release): full index 1000 159.0→159.9ms, 10000 307.5→306.1ms, sync no-op 10k 83.3→88.6ms (шум), search p95 573→590µs, cjk p95 7.9→4.9ms (вариативность) — регресса нет; golden/baseline не регенерировались.
Task SQ16: Review: spec PASS, quality CHANGES (1 Important: первый тест не достигал merge-пути) → fix round (oversize-тест + non-vacuity доказательство) → re-review ADDRESSED.
