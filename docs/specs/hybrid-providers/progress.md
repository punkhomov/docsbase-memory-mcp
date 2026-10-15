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
  Review (fresh): PASS/APPROVED, minors fixed in a follow-up commit (G1 default
  model line; rerank/embedding fixtures normalized to {request,status,response}).
- Task T2: complete (2026-10-14). `src/hybrid/{mod,config}.rs` (ungated),
  `{wire,openrouter}.rs` за feature `hybrid-remote` (reqwest/rustls-webpki-roots);
  `blocking::enter` вместо run_async/run_sync (design DELTA-T2: daemon уже роутит
  tool-вызовы через spawn_blocking); retry 4×250ms+джиттер; allowlist base_url;
  ключ redacted. Тесты: hybrid_config (4, дефолтная сборка) + hybrid_openrouter
  (12, фикстуры T1 через mock-транспорт, без сети).
- Task T3: complete (2026-10-14). Untagged `hybrid` (bool | таблица) с
  deny_unknown_fields и per-field дефолтами; `Config.hybrid_cfg`; validate:
  provider/window/dimensions → Admission; `docsbase config` JSON с ступенями
  через вынесенный `config_payload`. Тесты: +8 в tests/config.rs.
- Task T4: complete (2026-10-14). Plan-эмбеддинг в index-джобе; полный sweep в
  sync-джобе; авто-backfill при открытии проекта (`Shared::schedule_backfill`,
  `embed_coverage` в `status`); `search_docs_with` seam с окном
  max(limit, window), score = relevance, rerank=false → strict BM25, explicit
  Protocol на rerank=true без ступени; `rerank`-параметр в MCP-схеме; direct
  CLI через тот же seam. Тесты: unit-реранкер через seam (3) + hybrid_jobs (3).
- Task T5: complete (2026-10-14). CI-джоба `linux-hybrid` (feature clippy+сьют,
  SC-4 гейт без reqwest в дефолтном tree); `tests/hybrid_live.rs` (#[ignore]):
  live smoke и golden-замер — прогон 14.10: BM25 10/10 (nDCG 1.000) →
  rerank 10/10 (nDCG 1.000), паритет на golden-наборе (отчёт, не гейт, NFR-1).
- Task T6: complete (2026-10-14). Product design §7/§8 (reqwest-строка, feature,
  фаза-2 раздел), ADR-13 delta (dim/normalize в ключе, env-ключ, пины ревизий —
  принятое ограничение A3/R2), ADR-14 (итерация 1 целиком), CONFIG.md
  (hybrid-секции, семантика, пример), README EN/RU.
- 2026-10-14: итоговое ревью ветки (final review) — PENDING (свежий ревьюер,
  whole-branch diff).
- 2026-10-14: итоговое ревью ветки: CHANGES REQUIRED — 3 Important (Retry-After
  не внедрён; coverage считала вхождения, а не уникальные хэши → вечный backfill
  при дубликатах; невалидировались индексы embeddings-ответа) + 13 Minor.
- 2026-10-14: фикс-раунд (DELTA-T2.1): Retry-After с капом 30 с; coverage по
  уникальным chunk_hash и без построения клиента (только ключ кэша); валидация
  индексов (каждый ровно раз, в диапазоне) и явная ошибка на пустой `results`;
  plan-эмбеддинг до любых изменений состояния (ретраи watcher'а пере-планируют);
  fail-fast стадий в CLI index; явная ошибка на отсутствующий chunk text;
  truncation JSON-сообщений; таймаут 30 с; overrides материализуют дефолты;
  доки (OQ-2 закрыт, sketch §5, DELTA-T2.1). Новые тесты: Retry-After,
  исчерпание ретраев, 401/402/413, транспортные ошибки, валидация индексов,
  пустой rerank, границы embed_batches, дубликаты текстов в coverage.
  minor (deferred): p95-числа NFR-1 в отчёт (live-замер даёт порядок величины,
  формального p95 нет — вне гейта).
- 2026-10-15: ревью PR #41 (punkhomov): NEEDS CHANGES — исправлено:
  error-фикстуры нормализованы в {request,status,response} (единый парсер для T2);
  FR-1/FR-2 уточнены (порядок по advisory-`index`, `dimensions` из конфига,
  `model` в ответе не сверять — провайдер регистро/префикс-нормализует);
  `top_n=999` подкреплён живой пробой: 200, возвращаются все 4 дока
  (`tests/fixtures/openrouter/rerank/topn999-clamp.json`).
  Решения за человеком (вне репо): 8b-дефолт с ценой $0.01/M принят в T1-спайке;
  правки внешнего agreement (`rerank-3.5`→`v3.5`, `@d<dim>`→`@d<dim>@{l2,raw}`) —
  за оператором.
- 2026-10-15: ревью PR #40 (punkhomov): PASS в стеке 40+41 (по отдельности не
  мержить); FR-7 переформулирован как предварительный бюджет (калибровка при
  первом инциденте); согласие по цене 8b — за оператором; `rerank-3.5` в
  agreement — за оператором.
- 2026-10-15: ревью PR #42 (punkhomov): PASS, действий не требуется.
- 2026-10-15: ревью PR #43 (punkhomov): PASS with notes — сквош fixup-дубликата,
  doc-ниты `l2_normalize`/`citation_for` (исправлено); опциональный `index`
  у embeddings сделан advisory и в коде.

- 2026-10-15: план итерации 2 (dense + RRF) зафиксирован: DELTA-3 (FR-15…FR-18),
  design DELTA-7, задачи T7–T12; ветка `feat/hybrid-iteration-2` от свежего main
  (в неё влит #43 из `feat/hybrid-t1-spike` — base #43 не перецелился на main).
- Task T9: complete (2026-10-15). Схема v2: `projects.index_generation`
  (аддитивная миграция; re-read версии под BEGIN IMMEDIATE чинит гонки и потерянный
  version-write, ALTER идемпотентен через пробу колонки). `repo::{chunk_hashes,
  index_generation,bump_index_generation}`; инкремент поколения — в R2-транзакции
  `apply_plan` и только при непустом плане/удалениях (no-op sync не инвалидирует).
  Dense-кэш `hybrid::dense::{DenseCache,DenseIndex,DenseRows}`: ленивое построение,
  инвалидация по поколению, cap-деградация (`DENSE_CACHE_MAX_VECTORS=1_000_000`,
  стриминговый `top_k_iter` с bounded-хипом). Тесты: миграции v1→v2 + lost-version
  repair + конкурентность, `chunk_hashes` согласован с `chunk_texts`, поколение
  растёт только при применении плана, провал hybrid-плана (offline) не трогает
  поколение, кэш hit/rebuild/cap. Тестовые хардкоды schema=1 переведены на
  `migrations::SCHEMA_VERSION` (admission/store/cli_read/mcp_registry/mcp_frontend/
  install). minor (deferred): status-выход не показывает `index_generation`
  (внутренний маркер).
- Task T10: complete (2026-10-15). `Ranking {lexical, rrf}` в HybridCfg/
  HybridStages; `[hybrid] ranking` (неизвестное значение — ошибка парсинга);
  «rrf требует embedding» — Admission в validate и from_config (до feature-проверки,
  конфиг-ошибка важнее сборки). `search_docs_with(stages, dense, ...)`: RRF-путь
  (dense-кандидаты через DenseCache или стриминг поверх VectorCache при cap,
  `rrf_merge`, fetch=max(limit,window)), rerank поверх fused-списка,
  score=RRF-скор без rerank (FR-18). `Shared.dense`; CLI direct и `docsbase config`
  (`hybrid_config.ranking`) обновлены. Публичная библиотечная поверхность:
  `store::repo`, `Db::{connection,begin_write}` (конституция 1.3.0; понадобилось
  интеграционным тестам). Тесты: +7 `tests/hybrid_ranking.rs` (lexical без dense,
  RRF-порядок/скоры, RRF+rerank, rerank=false, окно при limit=1, нулевое покрытие,
  частичное), +3 `tests/config.rs` (парсинг/дефолт/ошибка), +2 `tests/hybrid_config.rs`
  (rrf без embedding в обоих режимах сборки). clipy/fmt — оба билда чисто.
