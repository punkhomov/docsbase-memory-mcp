# Progress: hybrid-providers

## Ledger

- 2026-10-08: черновик итерации 1 (requirements/design/tasks, ревизия 1).
  Ревью: NEEDS FIXES — 7 блокеров (`[hybrid]` TOML vs текущий `hybrid: bool`,
  поведение feature-off при `hybrid=true`, отсутствие backfill-политики,
  неопределённый fusion-контракт, двойной `/v1` в base_url,
  `rustls-tls-manual-roots` без рутов, `dim`/L2 не входят в ключ кэша).
- 2026-10-14: ресёрч (OpenRouter embeddings/rerank API, каталог моделей и цены;
  локальные кандидаты; ANN-опции `hnsw_rs`/`sqlite-vector-rs`) и сверка с кодом
  (T53-контракт `Embedder`+`VectorCache` жив, `hybrid: bool`, нет `[features]`,
  нет `reqwest`, `deny_unknown_fields`). Ревизия 2 спеки: закрыты все 7 блокеров
  (FR-5 ключ с dim+normalize; FR-11 fusion-контракт; FR-13 untagged TOML;
  FR-14 feature-off → Admission; FR-15 backfill; design §7 webpki-roots;
  везде `/api/v1` без удвоения), внесены факты ресёрча (`input_type`,
  `relevance_score`, `search_units`, таблица кодов, RRF k=60 — итерация 2).
  Scope подтверждён: remote-only (OpenRouter), локальные модели — итерация 2+.
- 2026-10-14: ревизия 2 на свежем ревью. Итог: все 7 блокеров CLOSED
  (paper-closure проверена, code-drift спот-чеки пройдены), но 6 Important и
  16 Minor — протоколы: (I1) feature-off Admission жил за feature-gate, а тест
  обязан идти в дефолтной сборке; (I2) хук backfill был назначен
  `lifecycle.rs`, тогда как открытие проекта — `server.rs`, и search-путь не
  прокидывал Config; (I3) `Handle::try_current` не отличает `spawn_blocking`
  (guard нереализуем); (I4) окно `max(limit,50)` vs провайдерский `top_n`
  (limit > top_n недостижим); (I5) параллелизм ×8–16 заявлен, но не имеет
  компонента; (I6) SC-2/SC-6 не имеют injection-seam в daemon-путь.
- 2026-10-14: ревизия 2.1 — I1–I6 закрыты (ungated `hybrid::config`;
  хук backfill → `server.rs`; `hybrid::blocking` с thread-local маркером;
  `window` вместо `top_n` у нас, `top_n = window` у провайдера; sequential
  батчи + golden-замер в NFR-1/T5; seam `search_docs_with`), Minor 7–22
  закрыты в доках (CLI `--hybrid` удалён; заголовки; `enabled` default;
  замена целиком; пустой candidates; источник текста; YAGNI `model_id`;
  feature-gated тест-файл; ADR-14 в T2; дефолт FR-8; LRU/эвикция R7;
  именование `HybridCfg`; JSON-тест `docsbase config`; ADR-13 delta).
- 2026-10-14: re-review ревизии 2.1. Итог: 21 из 22 findings ADDRESSED;
  NOT ADDRESSED — Minor (остаточный `model_id` в блоке Interfaces requirements) →
  исправлено сразу, штампы ревизии обновлены на 2.1. New breakage: none.
- 2026-10-14: ревизия 2.1 готова к коммиту (PR: спека целиком, docs-only).
- Task T1: complete (2026-10-14). P0-spike против живого OpenRouter (тестовый
  ключ, ~$0.002): OQ-1 закрыт фактами (`DELTA-T1` в requirements); дефолт-модель
  сменена на `qwen/qwen3-embedding-8b`@1024 (0.6b отсутствует в каталоге);
  нормированность/`input_type`/MRL/коды ошибок зафиксированы; фикстуры —
  `tests/fixtures/openrouter/` (embeddings: qwen3-8b 4-язычный, pplx raw,
  каталог, 2 ошибки; rerank: v3.5 top_n=2, 4-pro, top_n-клампинг, 2 ошибки).
  Ruling: rerank `top_n = window` у провайдера клампится к числу документов —
  наш срез до `limit` остаётся единственным местом лимита; пустые `documents`
  не отправляем (провайдер отвечает 400). minor: 429/5xx не провоцировались
  (политика по докам, пересмотр при первом инциденте).
