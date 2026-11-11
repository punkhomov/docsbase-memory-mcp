# SciFact test: Syntax / Literal, 2026-11-11 (#87)

Это измеренный advisory comparison явных режимов, не новая frozen baseline и не
результат стандартного BEIR document leaderboard. Оба отчёта содержат все300
positive-qrel test cases в исходном виде; ошибки входят в denominator. Метрики
считаются по top10 **citation slots**, duplicate document gain учитывается один
раз, unjudged не объявляется incorrect.

- [Полный Syntax JSON](scifact-test-2026-11-11-syntax.json)
- [Полный Literal JSON](scifact-test-2026-11-11-literal.json)
- [Исторический отчёт](scifact-test-2026-11-08.json) сохранён byte-identical.

## Результат и пределы

| Метрика | Syntax | Literal |
|---|---:|---:|
| Queries | 300 | 300 |
| Parser/query errors | 7 | 0 |
| Hits at 10 citation slots | 211 | 217 |
| Misses (including errors) | 89 | 83 |
| Mean recall@10 | 0.689222 | 0.707556 |
| MRR@10 | 0.486970 | 0.500399 |
| nDCG@10 | 0.531417 | 0.545737 |

Syntax summary полностью совпал с историческим отчётом, включая те же7 query
errors. Literal исполнил все300 запросов без query errors. Из7 ранее падавших
строк6 теперь имеют релевантное попадание;1280 остаётся retrieval miss при
непустой выдаче. Это не исправление всех проблем retrieval.

| Исходный query ID | Syntax | Literal | Literal reciprocal rank |
|---|---|---|---:|
| 1232 | query error | hit | 0.500000 |
| 1280 | query error | miss | 0.000000 |
| 1320 | query error | hit | 0.500000 |
| 279 | query error | hit | 1.000000 |
| 42 | query error | hit | 1.000000 |
| 744 | query error | hit | 0.500000 |
| 993 | query error | hit | 1.000000 |

Режим Literal явно меняет значение punctuation/grouping, поэтому изменения
не ограничены7 ошибками. Метрики изменились у18 cases; остальные282 совпали.
Ниже все11 изменившихся cases, которые и раньше не имели ошибки парсинга.
Из них1041/1259 приобрели hit,514/551 потеряли hit. Потери514/551 сохранены в
отчёте; Literal не обещает доминировать над Syntax на каждом запросе. По этим
наблюдениям ranking/boosts/окна не тюнились, queries/qrels не переписывались и
cases не исключались. Default Syntax и frozen gate остаются прежними.

| Query ID | Hit Syntax → Literal | Reciprocal rank | Duplicate documents |
|---|---|---|---|
| 1041 | miss → hit | 0.000000 → 0.142857 | 0 → 0 |
| 1259 | miss → hit | 0.000000 → 1.000000 | 2 → 2 |
| 1274 | hit → hit | 0.500000 → 1.000000 | 0 → 0 |
| 171 | hit → hit | 0.250000 → 0.125000 | 2 → 1 |
| 514 | hit → miss | 1.000000 → 0.000000 | 2 → 2 |
| 516 | hit → hit | 1.000000 → 1.000000 | 3 → 4 |
| 528 | hit → hit | 1.000000 → 0.500000 | 1 → 0 |
| 551 | hit → miss | 0.100000 → 0.000000 | 1 → 0 |
| 768 | miss → miss | 0.000000 → 0.000000 | 4 → 3 |
| 784 | hit → hit | 0.500000 → 0.250000 | 0 → 0 |
| 845 | hit → hit | 0.250000 → 0.111111 | 0 → 0 |

Top1 unjudged: Syntax178, Literal182; это отсутствие positive/negative annotation,
а не доказательство ошибки результата. SciFact состоит из scientific claims и
abstracts; измерение не доказывает качество поиска по технической документации.

## Одинаковые входы и происхождение

5183 documents,1109 raw queries,300 identical evaluated positive-test IDs;
split/evaluated/excluded/zero-only IDs и raw/derived hashes совпали с историческим
отчётом. Исходные7 строки, включая `baise`, не исправлялись.

| Raw input | BLAKE3 |
|---|---|
| corpus.jsonl | `b31ea992320a459f1775e129401b42624e8521c3b77111e7feea9c5b8f968910` |
| queries.jsonl | `71168a3205aa58fd9e9a6ad50b85d9fdbec37e3d767f89d5041d955ea8df2e64` |
| qrels/test.tsv | `40fd0aeb9b71e6798cfe1aa187ab995b129cc48464aab60b61e4ec2b2a7b873c` |
| Derived Markdown | `860c7eb54515f55209a3286e76ebd7590febf7abbddb465138a04ba6e039cce6` |

Источник фактической загрузки: [pinned public mirror](https://github.com/RichmondAlake/agent_memory_course/tree/e63c73afbb5fd3a9a6dafb00adec7d26abbdefff/part2/datasets/scifact),
commit `e63c73afbb5fd3a9a6dafb00adec7d26abbdefff`.
Время загрузки `2026-11-11T04:50:36.314565+00:00`. Canonical BEIR host вернул CONNECT403;
загружены только raw files из mirror, их3 BLAKE3 точно совпали с прежним отчётом.
Архив с canonical host не объявляется скачанным. Корпус в репозиторий не добавлен.

Wadden et al., Fact or Fiction: Verifying Scientific Claims (EMNLP2020), https://arxiv.org/abs/2004.14974. Claims/evidence annotations CC BY4.0; S2ORC abstracts ODC-By1.0. Upstream license: https://github.com/allenai/scifact/blob/a5254a1bf108726a1704d8e191b8a7f2225a029f/LICENSE.md. Raw files retrieved from pinned public mirror and verified byte-identical to the previous BEIR report; corpus/queries/qrels remain local.

## Runtime, harness и воспроизведение

Оба отдельных процесса запущены после чистого reviewed source/harness commit
`645485b739b042e468d006dd7288c0bb75a47e8a`. Перед/после обоих запусков проверены gitHEAD,
чистота worktree и SHA256 source/test/Cargo inputs. Runtime BLAKE3 независимо
пересчитан по sorted src/Cargo map; harness BLAKE3 — по length-prefixed5files и
сверен с compiled report. Metadata/report commit идёт после измерений и не меняет
runtime/harness. ADAPTER_VERSION1 и TOKENIZER_VERSION7 прежние: conversion и
analyzers не менялись. Mode хранится в `provenance.query_mode`.

- Runtime source hash: `78273be6c99e86312553ab713c68f7b3ad7e2734c9a317fdaa8d7dc6016b1aa2`.
- Harness source hash: `f762143713616a72d55129ab51e409209865f01b10666d313dee91860d93e002`.
- Rust1.88.0, Cargo1.88.0, release, `cli-full`, Linux x86_64.
- Configs: только Bm25; оба provider implementations скомпилированы, не активированы.
- Network-disabled isolation, cost cap$0, spent$0, provider requests0 в обоих отчётах.

```sh
# Один и тот же чистый source/harness checkout и локальный dataset.json/raw files.
# Parent существует; output paths новые, без перезаписи.
DOCSBASE_EXTERNAL_DIR=/absolute/path/to/scifact \
DOCSBASE_EXTERNAL_REPORT=/absolute/path/to/new-syntax.json \
DOCSBASE_EXTERNAL_QUERY_MODE=syntax \
DOCSBASE_EXTERNAL_CONFIGS=Bm25 DOCSBASE_BENCH_COST_CAP_USD=0 \
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features cli-full --release \
  --test search_bench_external external_measurement -- --ignored --exact --nocapture
# Повторить с query mode literal и другим output path.
```

Latency — одно наблюдение каждого режима на этой машине, без SLA/CI вывода:
Syntax p50=11.510ms,
p95=15.566ms;
Literal p50=13.609ms,
p95=23.267ms.
Это не доказательство performance regression/improvement: нет повторной
статистики и контроля ресурсов shared host. Frozen latency budget не менялся.
