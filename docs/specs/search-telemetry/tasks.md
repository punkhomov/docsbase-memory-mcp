# Tasks: search-telemetry (initial plan)

**Goal:** вызывающий узнаёт, на какой шкале `score`, куда ушло время, в каком
состоянии индекс и почему тир деградировал — не меняя форму ответа по умолчанию.
**Architecture:** `explain`-аргумент на MCP/CLI-границе, `SearchMeta` в прод-коде,
`AnnOutcome` (последний исход ANN) пишется путём поиска и читается `status`.
**Spec:** `docs/specs/search-telemetry/requirements.md` (rev 0.2, после ревью)
**Design:** `docs/specs/search-telemetry/design.md` (пишется вместе с T1)
**Status:** план; T0 — обязательный вход.

**Global constraints:**
- Rust edition 2024, MSRV 1.88; `cargo fmt`; clippy `--all-targets -- -D warnings`.
- Новых крейтов нет; default build остаётся offline (дерево-гейт NFR-1 продукта).
- В `src/**` нет `unwrap`/`expect`/`panic!`.
- **Форма ответа без `explain` не меняется** — `hits[]` побайтово прежний,
  `baseline.json` не перегенерируется, `PROTOCOL_VERSION` не двигается.
- Тайминги в тестах — гейт вместо `sleep` (Flake #9) и допуск `max(5 мс, 25 %)`.

**Review focus:**
1. Дефолтный ответ побайтово стабилен (SC-1) — это главный регресс-риск.
2. `stage_ms` вложены в `total`; неактивных стадий нет в словаре.
3. `AnnOutcome` ограничен: одна запись на проект, TTL, без роста памяти.
4. `status.ann` не делает дисковых чтений и не запускает билд.
5. Ни одно изменение не двигает ранжирование.

---

## T0 — Design + characterization test (обязательный вход)

**Goal:** зафиксировать сегодняшнюю форму ответа тестом **до** любых правок, чтобы
SC-1 проверялся, а не декларировался; заодно написать `design.md`.

**Files:**
- Create: `docs/specs/search-telemetry/design.md`.
- Modify: `tests/hybrid_ranking.rs` (фикстура четырёх конфигураций уже там —
  отдельный файл завёл бы вторую копию `project_with_docs`).

**Steps:**
1. Тест: `search_docs` без `explain` для всех четырёх конфигураций даёт ровно
   текущий JSON (снапшот через `serde_json::to_string`), включая порядок ключей
   элемента.
2. Тот же тест: `limit = 0` → `[]`; `Error::Query` → ошибка без `meta`.
   Плюс одна зафиксированная пара (запрос, конфигурация) сравнивается побайтово с
   сохранённой строкой — набор ключей без значений ловит только добавление поля,
   но не дрейф `score`/`lines`.
3. `design.md`: точки замера (`Instant` вокруг стадий в `search_docs_with`),
   структура `SearchMeta`, хранилище `AnnOutcome` (где живёт: `Shared`),
   маппинг имён на OTel, TTL/LRU.

**Acceptance:** тест зелёный **до** изменений и остаётся зелёным после каждого
следующего шага; `design.md` описывает точки замера и хранилище.

**Verify:** `cargo test --locked --test hybrid_ranking`

**Depends:** —

---

## T1 — `explain` на границе: MCP-схема, IPC-аргумент, CLI-флаг

**Goal:** FR-1/FR-8: аргумент существует, дефолт не меняется, конверт отдаётся
только при `explain: true`.

**Files:**
- Modify: `src/mcp/tools.rs` (schema `search_docs` + описание).
- Modify: `src/daemon/tools.rs` (`search_docs_with`: разбор `explain`, форма ответа).
- Modify: `src/cli/search.rs`, `src/cli/mod.rs` (`--explain`).

**Steps:**
1. RED: тест «`explain: true` → объект с ключами `hits` и `meta`»; сейчас падает.
2. `explain` парсится как bool; неверный тип — `Error::Protocol` (как `rerank`).
3. При `explain` ответ — `{hits, meta}`; `meta` пока минимальный
   (`score_scale`, `stages`, `window`, `index_generation`, `degraded: []`).
4. CLI: `--explain` печатает тот же конверт; без флага — прежний pretty-print.
5. `limit = 0` и пути ошибок — по FR-8.
6. Тест на **MCP-границе** (`tests/mcp_frontend.rs`): дефолтный результат
   `search_docs` — массив. Тест в `search_docs_with` этого не покрывает: конверт
   можно добавить в frontend-слое, мимо шва.

**Acceptance:** SC-1 (дефолт побайтово), SC-2 (шкала для 4 конфигураций), SC-5
(границы).

**Verify:** `cargo test --locked --test hybrid_ranking --test mcp_frontend --test cli_read`

**Depends:** T0

---

## T2 — `SearchMeta`: шкала, стадии, окно, generation

**Goal:** FR-2: поля названы и посчитаны из фактического исполнения, а не из
конфига.

**Files:**
- Create: `src/hybrid/meta.rs` (`SearchMeta`, `ScoreScale`, `Stage`, константы).
- Modify: `src/daemon/tools.rs` (заполнение).

**Steps:**
1. `ScoreScale`/`Stage` — enum'ы с `as_str()`; строки — константы (NFR-3).
2. `score_scale` берётся из фактически исполненного пути (rerank активен →
   `rerank`; иначе rrf → `rrf`; иначе `bm25`).
3. `stages` — только исполненные, в порядке исполнения.
4. `window` — фактический `fetch` после clamp до `MAX_HITS` (не `limit`).
5. `index_generation` — `repo::index_generation`.

**Acceptance:** SC-2; `window` в тесте с `limit: 5, window: 50` равен 50, а при
`window: 10_000` — 1000.

**Verify:** `cargo test --locked --test hybrid_ranking`

**Depends:** T1

---

## T3 — Тайминги по стадиям

**Goal:** FR-3: время атрибутировано, сумма не превышает общее.

**Files:**
- Modify: `src/daemon/tools.rs` (точки `Instant` вокруг bm25/dense/rrf/rerank).
- Modify: `src/hybrid/meta.rs` (`stage_ms`, `total`).

**Steps:**
1. RED: тест с гейтованным провайдером — `stage_ms.rerank >= задержка` в допуске
   `max(5 мс, 25 %)`; сейчас ключа нет.
2. Замеры вложенные: `total` — вокруг всего поиска, стадии — внутри.
3. Неактивная стадия отсутствует в словаре (не `0`).
4. Тест инварианта `sum(stage_ms[active]) <= total` на всех четырёх конфигурациях.

**Acceptance:** SC-3; тест не использует `sleep` для синхронизации.

**Verify:** `cargo test --locked --test hybrid_ranking`

**Depends:** T2

---

## T4 — `degraded`: токены деградации

**Goal:** FR-4: причина отката выражена стабильным токеном, а не текстом в лог.

**Files:**
- Modify: `src/hybrid/meta.rs` (`Degraded` enum + множество).
- Modify: `src/daemon/tools.rs` (`dense_candidates`: писать причину вместо/вместе с
  `eprintln!`).

**Steps:**
1. RED: тест «ANN откатился → `degraded` содержит `exact-fallback` и
   `ann-insufficient`»; сейчас пусто.
2. Токены: `ann-building`, `ann-insufficient`, `ann-error`, `exact-fallback`,
   `no-rerank-stage`.
3. `eprintln!` остаётся (лог демона) — но `degraded` становится контрактом.
4. Тест фиксирует полное множество токенов (защита от переименования).

**Acceptance:** FR-4; тест на каждую причину из реального пути.

**Verify:** `cargo test --locked --test hybrid_rank_ann --test hybrid_ranking`

**Depends:** T3

---

## T5 — `AnnOutcome` + `status.ann` + `index_generation`

**Goal:** FR-5/FR-6: `status` показывает состояние тира и причину, без билда и
дисковых чтений.

**Files:**
- Create: `src/hybrid/ann_outcome.rs` (хранилище: одна запись на проект, TTL, LRU).
- Modify: `src/daemon/server.rs` (`Shared` хранит `AnnOutcome`; `route_tool`
  передаёт в `status`).
- Modify: `src/daemon/tools.rs` (`status`/`project_entry`: `ann`,
  `index_generation`).
- Modify: `src/cli/status.rs` (direct-режим: `unknown` + `daemon not running`).
- Create: `tests/status.rs` (цели `--test status` до этого не существовало).

**Steps:**
1. RED: тест «после ANN-поиска `status.projects[0].ann.state == "ready"`»;
   сейчас поля нет.
2. Путь поиска пишет исход (`ready`/`building`/`too_large`/`fallback` + `reason`).
3. `status` читает запись; нет записи + hybrid off по конфигу → `disabled`;
   hybrid on, записи нет → `loading` + `reason: "no search yet"`; запись старше
   TTL → `stale`; `None` (нет кэша в вызывающем коде) → `unknown`.
4. `vectors`/`estimated_bytes` — из резидентного `AnnIndex`, иначе `null`.
5. `index_generation` — из `repo::index_generation`, тест сравнивает с БД.

**Acceptance:** SC-4; тест на `ready`/`fallback`/`disabled`; `status` не дольше
текущего p95 (нет дисковых чтений).

**Verify:** `cargo test --locked --test status --test mcp_frontend --test hybrid_ann`

**Depends:** T4

---

## T6 — OTel-имена как таблица

**Goal:** FR-10: имена полей совместимы с OTel GenAI conventions, и это
проверяется, а не декларируется.

**Files:**
- Modify: `src/hybrid/meta.rs` (константы имён + таблица «стадия → `gen_ai.*`»).
- Unit-тест: каждая стадия имеет пару; пары уникальны.

**Acceptance:** тест пары; в спеке §12 OQ-4 (trace_id) остаётся закрытым.

**Verify:** `cargo test --locked --lib hybrid::meta`

**Depends:** T2

---

## T7 — Документация и CHANGELOG

**Goal:** FR-9: контракт описан там, где его читают.

**Files:**
- Modify: `docs/MCP.md` (`explain`, поля `meta`, новые поля `status`, раздел
  Scores — сослаться на `meta.score_scale`).
- Modify: `CHANGELOG.md` (аддитивное добавление `explain`; не ломающее).
- Modify: `docs/specs/search-telemetry/progress.md` (леджер, первая запись).

**Acceptance:** `grep search_docs docs/MCP.md` совпадает с живой схемой (этот же
тест-инвариант, что в #57); CHANGELOG содержит `explain`.

**Verify:** `cargo test --locked --test mcp_docs`; гейт `docs` локально.

**Depends:** T1, T5

---

## T8 — Замер накладных расходов и финальная проверка

**Goal:** NFR-1 и полный прогон: телеметрия не стоит заметного времени, гейт
зелёный без перегенерации.

**Files:** нет новых; только замер и запись в `progress.md`.

**Steps:**
1. `cargo test --locked --release --test perf_budget` до/после (ветка vs `main`).
2. `cargo test --locked --test search_bench` — `metrics_match_baseline` зелёный,
   `baseline.json` не изменён (`git diff --stat` пуст).
3. Полная сюита + combo-фичи + clippy + fmt.
4. Запись в леджер: числа, `Ruling:` по допуску `max(5 мс, 25 %)`.

**Acceptance:** накладные ≤ 1 мс p95; baseline не тронут; CI зелёный на всех шести
джобах.

**Verify:**
```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --release --test perf_budget
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test search_bench
RUSTUP_TOOLCHAIN=1.88.0 cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

**Depends:** T1–T7

---

## Порядок и оценка

| Task | FR/SC | Оценка |
|---|---|---|
| T0 design + characterization | SC-1 (вход) | 0.5 d |
| T1 `explain` на границе | FR-1, FR-8 / SC-1, SC-2, SC-5 | 0.5 d |
| T2 `SearchMeta` | FR-2 / SC-2 | 0.3 d |
| T3 тайминги | FR-3 / SC-3 | 0.3 d |
| T4 `degraded` | FR-4 | 0.3 d |
| T5 `AnnOutcome` + `status` | FR-5, FR-6 / SC-4 | 0.6 d |
| T6 OTel-таблица | FR-10 | 0.1 d |
| T7 доки + CHANGELOG | FR-9 / SC-7 | 0.2 d |
| T8 замер + прогон | NFR-1 | 0.2 d |

Итого ≈ 3 d. T0 обязателен: без characterization-теста SC-1 — обещание, а не
проверка.

## Что этот спек НЕ делает (и где это живёт)

| Работа | Где |
|---|---|
| `cost_usd`, канал usage в провайдерах | спек `search-cost` |
| окно rerank, `rrf_k`, веса, адаптивные пороги | спек `search-cost` |
| snippet, dedupe по пути, `path_prefix` | спек `search-ux` |
| OTel-экспортёр, трейсы | отдельный ADR/спек |
| saturation на rank-1, live fail-on-skip | дельта `search-benchmark` |
| 75k ANN в nightly с утверждаемыми бюджетами | дельта `ann-index` |
