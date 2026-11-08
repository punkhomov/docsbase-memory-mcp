# Абсолютное качество frozen suite — 2026-11-08

После adjudication #70 (main 3503f2e). Зелёный gate означает отсутствие
новых регрессий и stale allowances, а не полноту retrieval.
Четыре retained rank1-trap allowances относятся к отдельным config/case
парам; они не являются числом misses или всех плохих ответов.

| Config | Hit@10 | Misses@10 | Mean recall@10 | Mean nDCG@10 | Top1 relevant | Top1 trap | Top1 unjudged |
|---|---:|---:|---:|---:|---:|---:|---:|
| Bm25 | 135/193 | 58 | 0.6800 | 0.5926 | 96 | 1 | 74 |
| Bm25Rerank | 141/193 | 52 | 0.7042 | 0.6691 | 120 | 1 | 50 |
| Rrf | 165/193 | 28 | 0.8338 | 0.6376 | 85 | 1 | 107 |
| RrfRerank | 183/193 | 10 | 0.9252 | 0.8553 | 149 | 1 | 43 |

Hit — хотя бы один размеченный evidence с path/line overlap. Recall —
доля найденных размеченных evidence, nDCG — их качество/позиции.
Top1 relevant — верхний результат совпал с positive; trap — с explicit
negative. Unjudged не доказывает ошибочность: нужна проверка разметки,
как в #70. Это не оценка качества ответа последующей LLM.

## Общие misses всех четырёх конфигураций

| ID | Query | Семейство | Решение |
|---|---|---|---|
| `enm-14` | went | en_morph | [#48](https://github.com/punkhomov/docsbase-memory-mcp/issues/48): irregular morphology; не исправляется в #72. |
| `enm-18` | datum | en_morph | [#48](https://github.com/punkhomov/docsbase-memory-mcp/issues/48): irregular morphology; не исправляется в #72. |
| `mix-18` | пробы живости демон завис | mixed_lang | Accepted limitation: cross-language/paraphrase retrieval текущих записанных моделей; ranking не меняется. |
| `mix-20` | постепенный раскат когорты рубильник | mixed_lang | Accepted limitation: cross-language/paraphrase retrieval текущих записанных моделей; ranking не меняется. |
| `mix-26` | переключатель остановка раскатки | mixed_lang | Accepted limitation: cross-language/paraphrase retrieval текущих записанных моделей; ranking не меняется. |
| `phr-12` | folding pending edits together | phrase | Accepted limitation: paraphrase retrieval текущего pipeline и записанных моделей; ranking не меняется. |
| `phr-13` | silent socket expiry | phrase | Accepted limitation: paraphrase retrieval текущего pipeline и записанных моделей; ranking не меняется. |
| `phr-14` | dormant link severance | phrase | Accepted limitation: paraphrase retrieval текущего pipeline и записанных моделей; ranking не меняется. |
| `phr-16` | freight train departure approval | phrase | Accepted limitation: paraphrase retrieval текущего pipeline и записанных моделей; ranking не меняется. |
| `phr-17` | even-handed arbitration | phrase | Accepted limitation: paraphrase retrieval текущего pipeline и записанных моделей; ranking не меняется. |

Accepted limitation — явное состояние текущей frozen оценки, не запрет
будущих улучшений. Эти десять запросов нельзя свести к единому дефекту
морфологии. Исправления требуют отдельного решения и golden evidence.
Выводы о качестве новых моделей по этим известным примерам не делаются.
Внешний advisory track проверяет другие документы/запросы.

Машинная диагностика строится из текущего прогона в gate-report.json:
config/slice counts, miss IDs, пересечение только реально запущенных modes,
accepted misses (miss и раньше), new misses (раньше hit) и unclassified
misses (нет сравнимого baseline case). Regression verdict использует
прежние правила и не читает diagnostics; baseline не перезаписывается.
