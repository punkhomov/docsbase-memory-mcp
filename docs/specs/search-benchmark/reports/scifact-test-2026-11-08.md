# SciFact test — первый внешний advisory baseline, 2026-11-08

Полный [JSON report](scifact-test-2026-11-08.json) содержит все 300 per-case
результатов. Это независимый от авторского frozen corpus набор scientific
abstracts; он не является проверкой качества сайта на project docs.

## Данные и воспроизводимость

- SciFact / BEIR, archive MD5 `5f7d1de60b170fc8027bb7898e2efca1`.
- Источник: https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip.
- Wadden et al., *Fact or Fiction: Verifying Scientific Claims*, EMNLP 2020,
  https://arxiv.org/abs/2004.14974. Claims/annotations — CC BY 4.0;
  S2ORC abstracts — ODC-By 1.0. Лицензии проверены до загрузки:
  [pinned license](https://github.com/allenai/scifact/blob/a5254a1bf108726a1704d8e191b8a7f2225a029f/LICENSE.md).
- Все 5 183 документа, test split: 300 запросов с positive qrels, k=10.
  В общем queries.jsonl 1 109 запросов; 809 не относятся к оцениваемому
  positive test split и перечислены как excluded. Zero-only queries: 0.
- Adapter 1, harness bench_version 1, TOKENIZER_VERSION 7; исходные
  файлы/корпус остаются вне репозитория. В error diagnostics включены
  семь отклонённых query texts с атрибуцией выше.
- Engine revision `0742f17308c5782331fcd39afb8a21ce23361e1e` (production tree тот же,
  что после #70). Точный runtime source hash и harness source hash
  фиксируют используемые bytes, включая новый runner до task commit.
- Rust 1.88.0, release, Linux x86_64, BM25; model pins пусты.
  Provider requests 0, spend $0, записанный cap $2.

| Identity | BLAKE3 |
|---|---|
| corpus_hash | `b31ea992320a459f1775e129401b42624e8521c3b77111e7feea9c5b8f968910` |
| queries_hash | `71168a3205aa58fd9e9a6ad50b85d9fdbec37e3d767f89d5041d955ea8df2e64` |
| qrels_hash | `40fd0aeb9b71e6798cfe1aa187ab995b129cc48464aab60b61e4ec2b2a7b873c` |
| derived_corpus_hash | `860c7eb54515f55209a3286e76ebd7590febf7abbddb465138a04ba6e039cce6` |
| harness_source_hash | `4b909351fbc5edb13e087bbf67f50d07cee959f26dfb5d58a5acda75268a492d` |
| engine_source_hash | `4c5408d19ea9ee79decd99797993f254dc14b8f41c73d735ba0df783beea06d1` |

Запуск: инструкция и env contract в [external-track.md](../external-track.md).
Повторный полный release-прогон дал идентичные quality/ranks/errors всех
300 cases и тот же harness hash; отличаются только host timing values.
После review исправлена проверка полноты offline rerank recordings;
окончательный report получен новым полным release-прогоном. Его harness
hash обновился, а quality/ranks/errors всех 300 cases снова идентичны.
Первый запуск до уточнения error policy останавливался на Error::Query
для ID 1232. Затем изменён только runner: ошибки остаются в denominator.
Corpus, query texts, qrels, parser и ranking не изменялись.

## Результат

| Метрика | BM25 |
|---|---:|
| Hit@10 | 211/300 (70.33%) |
| Misses@10 | 89/300 |
| В том числе query errors | 7/300 |
| Misses без query error | 82/300 |
| Mean recall@10 | 0.689222 |
| Mean MRR@10 | 0.486970 |
| Mean nDCG@10 | 0.531417 |
| Top1 relevant | 115/300 |
| Top1 explicit nonrelevant | 0/300 |
| Top1 unjudged | 178/300 |
| Search p50 / p95, ms | 1.247 / 2.252 |
| Index/setup, ms | 1267.0 |

Метрики используют binary positive **document qrels в citation slots**:
chunk-дубликат занимает место, но не даёт повторный document gain;
IDCG ограничен k. Это не стандартный BEIR document leaderboard score.
SciFact размечает релевантность evidence к scientific claim, включая
поддержку и опровержение; retrieval hit не является проверкой истинности claim.
Unjudged означает отсутствие суждения, а не доказанную ошибочность ответа.
Локальные timing values advisory; ими нельзя менять CI latency budgets.

## Наблюдения и решения

1. QueryParser интерпретирует обычную пунктуацию как query syntax. Семь
   запросов отклонены до retrieval. Они явно видны с нулевыми метриками,
   общий denominator остаётся 300. В #72 parser не исправляется:
   создана отдельная [#87](https://github.com/punkhomov/docsbase-memory-mcp/issues/87)
   о literal-query policy и совместимости syntax.
2. У 82 валидных queries нет найденного positive document в citation top10.
   По неполным qrels нельзя объявить всю выдачу неверной или все misses
   одним дефектом морфологии. Это accepted limitation данного baseline;
   переносу в frozen предшествуют full-context adjudication и отдельное
   reviewed delta. Никакого tuning по SciFact не выполнялось.
3. Повторы документов: 292 duplicate slots, у 171/300 queries. Это
   диагностическое проявление chunk output; оно само по себе не доказывает
   причинность misses. Связанная задача [#68](https://github.com/punkhomov/docsbase-memory-mcp/issues/68)
   требует отдельной полноты/бюджета выдачи, не увеличения окна наугад.
4. В frozen suite действуют отдельные [accepted limitations](../quality-review-2026-11-08.md)
   и #48 для irregular morphology. Внешний результат не меняет их qrels,
   baseline, allowances или production scoring.

### Отклонённые queries

| Query ID | Error category |
|---|---|
| `1232` | QueryParser syntax error (original query/error in JSON) |
| `1280` | Unknown field `proteins` (colon interpreted as field syntax) |
| `1320` | QueryParser syntax error (original query/error in JSON) |
| `279` | QueryParser syntax error (original query/error in JSON) |
| `42` | QueryParser syntax error (original query/error in JSON) |
| `744` | QueryParser syntax error (original query/error in JSON) |
| `993` | QueryParser syntax error (original query/error in JSON) |

Следующий evaluation set ротируется: LoTTE forum или подходящее небольшое
подмножество CoIR после проверки лицензии/размера/qrels, ближе к техническим
запросам. SciFact остаётся первым advisory baseline для воспроизведения;
никакой новый trap автоматически не внесён в frozen suite. Dense/rerank на
реальном SciFact не измерялись и не сравнивались: этот отчёт — только BM25.
Поддержка explicit offline replay проверена synthetic end-to-end тестом.
Online recording не включено, никакого скрытого расхода нет.
