# Delta 2026-11-08 — Рабочий external track (#72)

Ниже сохранён исторический proposal. Реализованный контракт задают
EXT-72-1…6 в requirements.md, design delta и T18/T19; он уточняет proposal:
zero qrel — **явное nonrelevant**, отсутствие строки — **unjudged**.
Все положительные scores бинаризуются, внешние qrels не превращаются
в whole-document traps frozen suite. ExternalReport — отдельная typed
оболочка без неприменимых legacy/discrimination/fixture allowances.

## Ручной запуск

Сначала проверить лицензию выбранного источника, затем скачать и распаковать
корпус **вне репозитория**. BEIR directory содержит `corpus.jsonl`
(`_id`, optional `title`, `text`), `queries.jsonl` (`_id`, `text`) и
`qrels/test.tsv` (header `query-id\tcorpus-id\tscore`). Добавить
`dataset.json` с непустыми строками:

```json
{
  "dataset": "SciFact",
  "dataset_version": "BEIR archive MD5 5f7d1de60b170fc8027bb7898e2efca1",
  "source_url": "https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip",
  "downloaded_at": "2026-11-08T00:00:00Z",
  "license_note": "Claims/annotations CC BY 4.0; S2ORC abstracts ODC-By 1.0; Wadden et al. 2020."
}
```

Время загрузки указать фактическое. Лицензия SciFact:
[LICENSE.md, pinned revision](https://github.com/allenai/scifact/blob/a5254a1bf108726a1704d8e191b8a7f2225a029f/LICENSE.md).
Исходные IDs непрозрачны, loader создаёт собственные hash-named Markdown
файлы и хранит обратное отображение. Непустые запросы без positive qrels
выбранного split не оцениваются; все excluded/zero-only IDs видны.

```sh
mkdir -p target/search-bench
DOCSBASE_EXTERNAL_DIR=/absolute/path/to/scifact \
DOCSBASE_EXTERNAL_REPORT="$PWD/target/search-bench/scifact-test.json" \
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --release --features cli-full \
  --test search_bench_external external_measurement -- --ignored --exact --nocapture
```

Это advisory measurement, не CI/golden gate. Output parent должен существовать;
runner пишет завершённый JSON атомарно, не перезаписывает файлы. Внутри репо
разрешены только `target/search-bench/` и benchmark `reports/`; source dataset
защищён. Отдельный новый output path позволяет сохранить повторный замер.
BM25 default, без ключа/модели/сети. Report включает raw и derived hashes,
полный inventory IDs, исходные document IDs в per-case ranks, runtime source
hash, harness source hash и git revision, profile/platform, latency/cost.
Hash bytes имеют смысл вместе с adapter/tokenizer versions и source revision.

Дополнительные параметры: `DOCSBASE_EXTERNAL_SPLIT` (default `test`),
`DOCSBASE_EXTERNAL_K` (1…20, default 10), `DOCSBASE_EXTERNAL_CONFIGS`
(comma-separated `Bm25,Bm25Rerank,Rrf,RrfRerank`; список обязан включать
`Bm25`, остальные режимы добавляются явно) и
`DOCSBASE_EXTERNAL_REPLAY_DIR`. Не-BM25 требует explicit offline replay;
cap `DOCSBASE_BENCH_COST_CAP_USD` (default $2, finite nonnegative) записывается,
но **этот runner никогда не делает provider requests**, spend всегда $0.
Online recording/оплата inference не входит в evaluation runner.

## Delta 2026-11-11 — explicit query mode (#87)

`DOCSBASE_EXTERNAL_QUERY_MODE=syntax|literal` выбирает request interpretation;
без переменной действует owner default Syntax. Пустое/неизвестное значение
или не-Unicode строка — явная ошибка, без отчёта. Режим записывается в
`provenance.query_mode` и передаётся в product search и для warm-up, и для
каждого исходного запроса. Строки запросов и provider/replay identities не
переписываются; отсутствующее recording по-прежнему прерывает replay.

Для сравнения режимов запускаются два отдельных процесса на одном чистом
зафиксированном runtime/harness, одинаковых raw input hashes, split, IDs и k;
у каждого новый output path. Официальный профиль выше включает cli-full,
а выбранный Bm25 остаётся офлайн (spend0). Например, добавить перед командой
`DOCSBASE_EXTERNAL_QUERY_MODE=literal`; повторить с `syntax` и другим именем
отчёта. Не редактировать исходные запросы и не исключать query_error cases.

Парный SciFact замер #87 намеренно сохраняет полные300cases в обоих новых
JSON для проверки mode/error-count delta. Исторический отчёт остаётся прежним.
Это отдельный audit режима, без переноса SciFact qrels в frozen gate и без
тюнинга ranking. ADAPTER_VERSION1 прежний: raw→Markdown conversion не менялся;
execution определяется mode, engine revision/source hash и harness hash.

## Replay contract

В отдельном каталоге `identity.json` фиксирует:

```json
{
  "derived_corpus_hash": "BLAKE3 of materialized Markdown corpus",
  "queries_hash": "BLAKE3 of raw queries.jsonl",
  "adapter_version": 1,
  "tokenizer_version": 7,
  "embedding_model_pin": "provider/model@d1024@l2",
  "rerank_model_pin": "provider/model"
}
```

Пины соответствуют выбранным стадиям. Recordings имеют существующий формат:
`vectors/<pin_slug>/manifest.json` + named binary, `rerank/<pin_slug>.json`.
Loader отвергает несовпадающие identities/model/dimension, отсутствующие
query/chunk vectors и rerank pools. Каждый pool обязан иметь полный
набор уникальных scores для всех кандидатов: production append fallback
не должен превращать неполный replay в якобы измеренный rerank. Corpus hash в vector manifest должен
совпасть с derived hash; aggregate annotation hash не заменяет content keys.
Runner не создаёт отсутствующие записи и не понижает режим до BM25.

Для подготовки записей доступен существующий test-support API:
`ExternalDataset::load`, `bench::build_env`, `bench::record_fixtures_with`.
End-to-end тест `explicit_replay_runs_all_modes_and_missing_content_fails_closed`
показывает запись/повтор/ошибки с синтетическими providers без сети. Реальные
provider calls должны выполняться отдельным явно выбранным recording
оператором с cost cap; инструкция не разрешает неограниченный live вызов.

## Интерпретация и следующий датасет

Метрики — binary document relevance в **top-k citation slots** продукта.
Повторные chunks занимают slots, но document gain считается один раз;
IDCG ограничен k. Это не число из стандартного BEIR document-level leaderboard.
Error::Query остаётся отдельным query_error case с нулевыми метриками;
запрос не редактируется и не исключается из denominator. Остальные errors
(в том числе missing replay) прерывают весь отчёт.
Unjudged top1 не объявляется incorrect. Frozen hit/recall/nDCG и retained
allowances видны отдельно: [quality review](quality-review-2026-11-08.md).

Первый полный SciFact test report и выводы живут в `reports/`. SciFact —
scientific abstracts, а не project docs; по нему нельзя заключить, что
продукт хорошо работает на сайте или что новая модель лучше. Ranking по
результату не тюнится. До дальнейших изменений качества выбрать другой
независимый набор ближе к технической документации; LoTTE forum / CoIR
остаются кандидатами после проверки объёма, лицензии и qrels. Важные gaps
сначала требуют ручной full-context проверки, затем отдельного reviewed
решения о переносимом frozen case; автоматического переноса нет.

---

# External Benchmark Track — Proposal (не часть v1-гейта)

Статус: предложение операционного режима. Не меняет v1-гейт, его
требования, дизайн и задачи: всё ниже — либо переиспользование
существующего, либо явно помеченные будущие задачи отдельной итерации
(harness-расширения из раздела «Что понадобится»). DELTA-2 (пересмотр
баунда чанков по замеру 132 vs 500–1000) — отдельное решение автора спеки;
числа ниже (200–400) — лишь пример порядка, не предрешение.

## Проблема

Замороженный сьют (`tests/fixtures/search-bench/`) — регрессионный инструмент:
он доказывает «не сломали», но не измеряет обобщение (все кейсы известны,
overfit неизбежен). Объём и разнообразие корпуса нельзя растить раздуванием
синтетики: баунд 500–1000 чанков недостижим ручным авторством при сохранении
похожести на настоящие проектные доки (замер: 62 дока → 132 чанка).

## Подход: внешний zero-shot трек вручную

Второй трек рядом с замороженным сьютом. Отвечает на другой вопрос:
«а мы вообще хорошо ищем?» — на корпусах и запросах, которых мы не писали.

### Операционная модель

- **Ручной прогон, не CI.** Оператор (человек или агент) локально скачивает
  сет, прогонит harness, коммитит отчёт:
  `docs/specs/search-benchmark/reports/<dataset>-<дата>.json` + строка
  в ledger. Формат — тот же `Report` (включая `bench_version` и блок
  происхождения выше) с `track: "external"`; поля, применимые только
  к frozen-гейту (`legacy_parity`, `discrimination`, `fixture_version`),
  — null/опущены. Правило размера: первый (baseline) отчёт — полный
  per-case; следующие хранят дельты + полные кейсы только для
  разошедшихся/упавших (тысячи кейсов целиком в репо не лежат). Workflow по расписанию/вручную, PR не блокирует (как live-трек,
  FR-17/FR-23).
- **Ничего не вендорим в репо.** Корпуса живут локально у оператора
  (лицензии остаются локальной проблемой). В репо — только отчёт.
  Воспроизводимость — через зафиксированные в отчёте версии, а не через
  копию данных.
- **Отчёт — факт, решение — отдельно.** Report, not gate: регрессия на
  внешнем сете не валит разработку. Решение принимает оператор по правилу:
  воспроизводится на нашем пайплайне → заводится trap-кейс замороженного
  сьюта (по FR-26: reason, reviewed_by, Jaccard-гейт для semantic);
  не воспроизводится / шум выборки → остаётся наблюдением в ledger.
  Первый прогон по датасету = baseline; следующие сравниваются дельтами
  (та же философия, что FR-14 для замороженного сьюта).
- **Замыкание цикла.** Главный выход внешнего трека — не цифра, а новые
  регрессионные кейсы (см. правило выше). Явная строка против
  самоуничтожения смысла: внешний трек НЕ является основанием для
  изменения ранжирования — только frozen golden и NFR-6. Иначе через
  два прогона он станет вторым frozen suite (ротация наборов
  обязательна).

### Что переиспользуется без изменений

- Раннер конфигураций (`run_config`, 4 конфига) и реплей-стадии.
- Формат отчёта (`Report`: per-case ранги/метрики, срезы, дельты) +
  блок происхождения датасета (ниже).
- Cost cap (FR-18/NFR-7): действует тот же механизм
  (`DOCSBASE_BENCH_COST_CAP_USD`, дефолт `$2`); отдельное значение
  для внешнего трека оператор задаёт явно на прогон, дефолт не
  «определяется при первом прогоне», а наследуется.
- Валидация внешнего корпуса НЕ идёт через `Fixtures::validate`:
  баунды FR-1/FR-24 (число доков/чанков/кейсов) — контракт замороженного
  сьюта. Внешний корпус проверяется только sanity-правилами загрузчика
  (парсинг, непустота, детерминированный порядок); fail-closed
  относится к отсутствию векторов/скоров (FR-7 по аналогии).

### Что понадобится (будущие задачи, не сейчас)

- **Override корпуса.** Загрузчик читает не только захардкоженный
  `tests/fixtures/search-bench`, но и внешний каталог (`--corpus-dir`):
  корпус + запросы + qrels в BEIR-совместимой раскладке.
- **BEIR-адаптер.** Маппинг `query / corpus / qrels` → внутренние
  `QueryCase` (+ `grade 2` для релевантных; `0` — не грейд, а пометка
  «нет суждения», в `expected` не попадает; `absent` не строится —
  несуждённое ≠ нерелевантное, урок hole-анализа).
- **Блок происхождения в отчёте:** `dataset`, `dataset_version`,
  `source_url`, `downloaded_at`, `query_count`, `license_note` —
  плюс те же якоря воспроизводимости, что у замороженного отчёта
  (FR-12/FR-20): `bench_version`, версия адаптера/маппинга,
  `corpus_hash`, `queries_hash`, `model_pins`, версия harness и
  `TOKENIZER_VERSION`. Без них отчёт невоспроизводим и несравним.
  Сами queries/qrels — маленькие: при позволяющей лицензии вендорятся
  в репо рядом с отчётом, иначе хранятся только хэши + IDs. Корпуса
  (гигабайты) не вендорятся никогда.
- **Политика стоимости.** Прогоняются все конфигурации, которые оператор
  готов оплатить; lexical-only — обязательный минимум (бесплатно, всегда),
  а не норма. Dense/rerank — осознанно и под cap'ом (тот же механизм
  FR-18, значение на прогон). Replay-вектора для внешнего корпуса
  записываются один раз и переиспользуются следующими прогонами
  (амортизация вместо повторной оплаты); инвалидация — по тройке
  (`corpus_hash`, model pin, `TOKENIZER_VERSION`): смена любого
  компонента требует перезаписи. NFR-1/NFR-2/NFR-9 (30 с/12 MiB/
  линейный рост) — бюджеты frozen-гейта, на внешние прогоны не
  распространяются: их бюджеты — время и деньги оператора, фиксируемые
  в отчёте (latency/cost), а не гейт.

### Кандидаты и критерии отбора

Критерии (все обязательны): малый размер (тысячи, не миллионы документов —
иначе не проиндексировать локально за разумное время); лицензия,
допускающая локальное исследовательское использование; контент, близкий
к проектной документации / коду / техвопросам; наличие qrels.

Первый шорт-лист (проверить лицензии перед скачиванием):

- LoTTE-forum (StackExchange Q/A — ближе всего к «как спрашивают агенты»);
- SciFact / NFCorpus / FiQA (малые BEIR-сеты, отлаженный формат);
- Подмножество CoIR (кодовый поиск; наш identifier-слой).

Явно вне скоута трека: полный MS MARCO (8.8M пассажей — неиндексируемо
вручную за разумное время и деньги), сеты без qrels, сеты с запретом
на локальное хранение.

### Связь с DELTA-2

DELTA-2 фиксирует достижимый баунд замороженного сьюта (предложение:
200–400 по замеру 132 + T6-добавка). Внешний трек — это то место, где
растут объём и разнообразие, а не синтетический корпус. Аргумент
«добавить ещё 300 кейсов» после DELTA-2 отклоняется ссылкой сюда:
объём — во внешний трек, дискриминативность — в ловушки.

## Открытые вопросы (за оператором, не за harness)

- Каденс ручных прогонов (предложение: перед релизом + при смене
  модели/ранжирования).
- Cap стоимости внешнего прогона (отдельный от $2 live-трека).
- Критерий «выпускного» фаундинга: когда внешний провал превращается
  в regression-кейс, а когда остаётся наблюдением.
