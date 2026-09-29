# CBM (codebase-memory-mcp) — исследование

> Дата: 2026-09-29.
> Объект: [DeusData/codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp), ветка `main`, релиз v0.11.0 (2026-09-15).
> Темы: индексирование и retrieval (BM25 + «эмбеддинги» + слияние/ранжирование), git worktrees,
> интеграция с песочницами агентов (Docker Sandboxes / `sbx`).
> Документ — отдельный конспект; другие файлы проекта не затрагиваются.

## 0. TL;DR

- CBM — нативный C-бинарь (tree-sitter × 158 языков + Hybrid LSP для 11 семейств), пишет граф
  в SQLite **собственным page-level writer'ом** (`internal/cbm/sqlite_writer.c`), минуя SQL-парсер.
- «Эмбеддинги» — **не нейросеть в рантайме**, а вшитая статическая таблица токен-векторов
  (nomic-embed-code, 40 856 × 768 int8, ~31.4 MB) + Random Indexing/IDF. Модель нужна только
  для одноразовой дистилляции (`scripts/extract_nomic_vectors.py`).
- BM25 — SQLite FTS5 (contentless) с функциями `cbm_camel_split(name)` при вставке и весами
  `bm25(nodes_fts, 1.0, 1.0, 1.0, 1.0, 0.3)`; сверху аддитивные бусты за точное имя и тир label.
- `semantic_query` — векторный поиск по **min-cosine по каждому ключевому слову**; 11-сигнальный
  combined score к поиску не применяется (он только для рёбер `SEMANTICALLY_RELATED`).
- **Слияния (RRF/hybrid/cross-encoder) нет**: BM25, vector и структурный regex — три независимых
  списка с раздельной пагинацией; `query` и `semantic_query` взаимно исключены.
- Git worktrees: детект есть (`is_worktree`, `canonical_root` = корень основного репо), в графе
  `Project → HAS_BRANCH → Branch` с git-метаданными, discovery/watcher работают в worktree.
  Но каждый worktree-root = **отдельный проект/индекс**; ссылки на «оригинальный проект» нет.
- Docker Sandboxes (`sbx`): лучший вариант интеграции — CBM как host-side stdio MCP через
  host MCP gateway (`sbx mcp add cbm --command ...`), т.к. прямой mount сохраняет абсолютные
  host-пути и host-овый daemon/кэш переиспользуется. Clone/mountless режимы — только CLI внутри VM.

## 1. Индексирование

### 1.1 Пайплайн

- Discovery (ignore-слои: hardcoded → `.gitignore` → `.cbmignore`, symlink'и скипаются), парсинг
  tree-sitter, Hybrid LSP (Python, TS/JS/JSX/TSX, PHP, C#, Go, C/C++, Java, Kotlin, Rust) —
  lightweight C-реализация type-resolution.
- RAM-first: LZ4, in-memory SQLite, один дамп в конце; граф живёт в graph buffer
  (`src/graph_buffer/graph_buffer.c`), пассы наполняют его узлами/рёбрами.
- Пассы: definitions, calls, usages, semantic (INHERITS/DECORATES/IMPLEMENTS), similarity,
  semantic edges (эмбеддинги), route/k8s/infra и т.д. (`src/pipeline/pipeline.c`).

### 1.2 Запись в SQLite

- Дамп делает `cbm_write_db` (`internal/cbm/sqlite_writer.c`) — конструирует B-tree страницы
  напрямую; таблицы: `projects`, `file_hashes`, `nodes`, `edges`, `node_vectors`, `token_vectors`,
  `nodes_fts` (FTS5 contentless) + служебные.
- Схемы векторов (`sqlite_writer.c:2281–2286`):
  - `node_vectors(node_id, project, vector BLOB)` — int8 768d на узел;
  - `token_vectors(id, project, token, vector BLOB, idf INTEGER)` — enriched-вектор токена + IDF
    (fixed-point) для query-time lookup.

### 1.3 Семантический проход индексации

`src/pipeline/pass_semantic_edges.c`:

1. Токенизация имён/сигнатур/докстрингов: split camelCase/snake_case, lowercase, ~100
   сокращений (`ctx → context`, `req → request`, …) в `cbm_sem_tokenize`
   (`src/semantic/semantic.c`).
2. Корпус и IDF; co-occurrence Random Indexing в 2 прохода: окно ±5, прореживание частых
   токенов до ~512 вхождений, бленд pass2 `α=0.3/β=0.7`, тайлинг под L2 (`semantic.c`,
   `cbm_sem_corpus_finalize`).
3. Per-function RI-вектор = IDF-взвешенная сумма enriched-токенов, normalize, int8 768d
   (`phase4_build_and_store_vectors`) → `node_vectors`.
4. Enriched-токен-векторы с `idf > 0` → `token_vectors` (`phase3c_export_token_vectors`).
5. Источник вектора токена: pretrained-таблица nomic, если токен в словаре, иначе sparse random
   (8 ненулей, XXH3-seeded) — `cbm_sem_random_index`.
6. Для рёбер дополнительно: MinHash (64) + LSH, rotsq (RaBitQ-style 4-bit: FWHT-ротация
   768→1024, per-vector scale/offset, точный code-expansion IP) и 11-сигнальный combined score.

### 1.4 Инкремент и артефакт

- `pipeline_incremental.c`: file hashes (sha256/mtime), инкрементальный реиндекс; хэшируется и
  `is_worktree` (`:291`).
- Watcher — **git-поллинг** (`src/watcher/watcher.c:4`): `git status` + HEAD, adaptive 5–60 c,
  dirty-state signature (#937); non-git проекты не поллятся вообще.
- Team-артефакт `.codebase-memory/graph.db.zst` (zstd, `VACUUM INTO`, strip indexes): две
  градации (best — при явном index, fast — watcher). Импорт при наличии, если `persistence=true`.

## 2. Модель «эмбеддингов»

### 2.1 Что лежит в бинаре (`vendored/nomic/`)

- `code_vectors.bin` — 40 856 токенов × 768d, int8 (scale ×127), ~31.4 MB, вкомпилирован через
  `code_vectors_blob.S` (`.incbin`); `code_vectors.h` (`PRETRAINED_TOKEN_COUNT`, `PRETRAINED_DIM`,
  `pretrained_vec_at`), `code_tokens.{txt,h}` — словарь.

### 2.2 Как получена (`scripts/extract_nomic_vectors.py`)

- Модель `nomic-ai/nomic-embed-code` (7B), один прогон full inference на каждый токен
  (prefix `search_query: `), mean pooling, Matryoshka-усечение до 768, L2-norm,
  mean-centering против анизотропии, «simulated attention» (3 итерации, K=32, α=0.3 — бленд
  с топ-K соседями), int8 ×127. Одноразово, ~2–10 ч на GPU/CPU.

### 2.3 Как используется в рантайме

- Только lookup: `cbm_sem_random_index` берёт готовый вектор токена; для токенов вне словаря —
  sparse random. Никакого трансформера/ONNX/API в рантайме; вес бинаря ~40 MB, сеть не нужна.

## 3. BM25

### 3.1 FTS5-схема и токенизация (`src/store/store.c:371–500`)

- Contentless-таблица `nodes_fts(name, qualified_name, label, file_path, body)`, токенизатор
  `unicode61 remove_diacritics 2`.
- При вставке: `name` прогоняется через скалярную SQL-функцию **`cbm_camel_split`** (не
  tokenizer!); `body` = `json_extract(properties,'$.docstring')` как есть; qn/label/file — сырые.
- Legacy-совместимость: таблица без `body` продолжает работать; backfill деградирует по лестнице
  `{body,camel} → {body} → {camel} → {}` (`cbm_store_fts_rebuild`).

### 3.2 Запрос и веса (`src/mcp/mcp.c:3747+`)

- Свободный запрос режется на alnum/underscore-токены и склеивается через `OR` — FTS5-операторы
  и кавычки недоступны (инъекция невозможна); пустой результат → fallback на regex-путь.
- Веса: `bm25(nodes_fts, 1.0, 1.0, 1.0, 1.0, 0.3)` (BM25F-корректные, body ниже identifier'ов).
- Early-exit: inner subquery без JOIN/WHERE, `LIMIT 2000` кандидатов (`BM25_INNER_LIMIT`), затем
  join/filter/boost; счётчик total ограничен тем же окном, saturation флагуется.

### 3.3 Бусты и ранжирование

- К отрицательному `base_rank` добавляются: `name == query` → −30; `lower(name)==lower(query)`
  → −20; label-тиры Function/Method → −10, Route → −8, type-like/relations → −5.
- Исключения: `File/Folder/Variable/Project`; тай-брейк по `id` (стабильная пагинация).
- `search_mode: "bm25"` в ответе.

## 4. Векторный поиск (`semantic_query`)

- Аргумент — **массив уже разбитых ключевых слов** (строки), взаимно исключён с `query`.
- По каждому слову: enriched-вектор из `token_vectors` (int8 768d), иначе sparse random;
  normalize + int8; кандидаты предфильтруются по первому слову с запасом ×5
  (`fetch_limit`), затем финальный скор = **min cosine по всем словам** (`vs_min_cosine_score`),
  сортировка score DESC / node_id ASC (`cbm_store_vector_search`, `store.c:10158+`).
- Итеративное расширение окна, пока top-K префикс не сертифицирован (K-й скор строго выше
  лучшего отброшенного) — стабильные offset-страницы; ≤32 слов; `semantic_offset ≤ 99998`;
  только label'ы callable/type.
- Результат — отдельная секция `semantic` (`qn`, `label`, `file`, `score`); если заданы только
  `semantic_query` без фильтров, структурная страница не подмешивается.
- 11-сигнальный combined score здесь **не используется** — README вводит в заблуждение; он
  применяется только к рёбрам `SEMANTICALLY_RELATED` (см. §6).

## 5. Слияние результатов и ранжирование (fusion)

- **RRF/hybrid/reranker отсутствуют**.
- `query` → BM25 и немедленный возврат. `semantic_query` → vector-поиск (отдельная секция).
  Regex/структурные фильтры → отдельный список (`cbm_store_search`) со своими `offset/limit`.
- У каждого режима своя пагинация: `offset/limit` (структурный), `semantic_offset/semantic_limit`
  (векторный). `query` и `semantic_query` нельзя комбинировать в одном вызове — агент сам
  «сливает» ответы mental-моделью.
- Практический вывод для сравнения с docsbase: CBM не делает score fusion; «hybrid» здесь =
  два независимых движка рядом.

## 6. Приложение: 11-сигнальный combined score (рёбра `SEMANTICALLY_RELATED`)

`src/semantic/semantic.c:1654+`, веса по умолчанию (сумма ~1.0, proximity — множитель):

| Сигнал | Вес |
|---|---|
| TF-IDF (sparse cosine) | 0.20 |
| Random Indexing (rotsq IP) | 0.25 |
| MinHash (Jaccard) | 0.10 |
| API-сигнатуры | 0.15 |
| Type-сигнатуры | 0.10 |
| Decorator-паттерны | 0.05 |
| AST profile + data flow + Halstead | 0.10 |
| Data flow | 0.05 |

- Proximity-множитель: `[1.0, 1.10]` (тот же файл +10%, та же директория +5%); итог клампится
  в `[0,1]`.
- Если MinHash Jaccard уже ≥ порога `SIMILAR_TO` — score = 0 (не дублировать near-clone).
- Порог эмита `0.75`, ≤10 рёбер на узел (`CBM_SEM_MAX_EDGES`), graph diffusion α=0.3.

## 7. Git worktrees

- `src/git/git_context.c`: `is_worktree = (git_dir != git_common_dir)`; `worktree_root` =
  `rev-parse --show-toplevel`; `canonical_root` = корень **основного** репо
  (`--git-common-dir`; на git 2.31+ `--path-format=absolute`, иначе realpath-fallback).
  Issue #659 (неверный canonical_root для worktree/subdir) исправлен; инвариант закреплён
  `tests/test_git_context.c::canonical_root_linked_worktree`.
- Discovery: `.git`-файл gitlink (`gitdir: …`) разбирается; `.gitignore`/`info/exclude`/config
  читаются из общего common dir; `.worktrees`/`.claude-worktrees` — в always-skip.
- Граф: `Project → HAS_BRANCH → Branch` с QN `<project>.__branch__.<branch_slug|detached|working-tree>`
  и props `is_worktree`, `worktree_root`, `git_common_dir`, `canonical_root`, `branch`,
  `head_sha`, `base_sha` (`pipeline.c:pass_structure`).
- Инкремент/ watcher: `is_worktree` входит в incremental state (`pipeline_incremental.c:291`);
  watcher понимает `.git`-gitlink и dirty-state (#937).
- Ограничения: **каждый worktree-root = отдельный проект/БД** (имя проекта из полного пути,
  `fqn.c:415`; override только `name`); `canonical_root` — информационное поле (Status/Branch
  props), **не ссылка** на индекс основного репо, шаренного индекса и `git worktree list` нет.
- Диагностика: `index_status(verbose=true)` показывает worktree/shadow Git-пути,
  `list_projects` — branch; `detect_changes` работает с uncommitted worktree-байтами.

## 8. Docker Sandboxes (`sbx`)

### 8.1 Как устроен `sbx`

- `sbx` — CLI Docker Sandboxes: агент исполняется в **microVM** (свой Docker daemon, FS, сеть).
- Workspace:
  - прямой mount (`sbx run` из каталога) — filesystem passthrough, **абсолютный путь совпадает
    с host**;
  - clone mode — host-репо ro в `/run/sandbox/source`, работа в приватном клоне внутри VM;
  - mountless — workspace только внутри VM.
- Состояние sandbox'а персистится между stop/restart до `sbx rm`.
- **MCP gateway — на хосте**: серверы регистрируются один раз (`sbx mcp add`), подключаются через
  `--static-mcp`, `sbx mcp load` или динамически (`mcp-find`/`mcp-add`). Local stdio-серверы
  запускаются **на хосте, вне изоляции песочницы**. Поддерживаемые агенты с gateway на старте:
  Claude Code, Codex, Devin, Gemini, Kiro, OpenCode. User-level конфиги агентов внутри sbx
  не читаются — только project-level в рабочем каталоге.
- Сеть: весь outbound через host-прокси с политиками; governance Cedar для MCP (`register`,
  `invokeTool`; тип `local-stdio`, identity = путь к исполняемому файлу).

### 8.2 Рекомендуемая схема CBM + sbx

```console
sbx mcp add cbm --command /usr/local/bin/codebase-memory-mcp
sbx run opencode --static-mcp cbm          # или dynamic + mcp-add
```

Почему это удачно:

- Прямой mount сохраняет host-путь → агент присылает CBM тот же путь, который видит host-индекс;
  проблема devcontainer path mismatch (#822) в sbx **не возникает**.
- Используются **host-овый daemon и host-овый кэш** CBM: один индекс шарится между хостовыми
  сессиями и всеми песочницами; правки агента через passthrough видны git-поллингу watcher'а →
  авто-реиндекс.
- Не нужны uid/volume-настройки для кэша, `CBM_IN_PROCESS`, AF_UNIX-разрешения; сеть CBM не
  использует (при `--command` регистрация даже без сети/Docker).
- Индексация идёт на host CPU/RAM — лимиты microVM её не ограничивают.

Оговорки:

- Docker предупреждает: host-side stdio-сервер — вне изоляции песочницы (доступ к host-ФС).
  Ограничивать `CBM_ALLOWED_ROOT` каталогом проекта; `persistence=false` (дефолт MCP) чтобы
  не писать `.codebase-memory/` в репо.
- Governance: org-политика должна разрешить `register` + `invokeTool`; `resource.command/args`
  могут ограничивать; `@requireApproval` для `sbx mcp add` не поддерживается.
- Бинарь CBM должен быть установлен **на хосте** (macOS/Windows/Linux), не в VM.

### 8.3 Clone mode и mountless

- Host-side CBM видит оригинальное репо, а агент правит приватный клон внутри VM → результаты
  CBM не соответствуют рабочему дереву агента. Через gateway CBM в этих режимах бесполезен.
- Обходной путь: запечь CBM в template/kit песочницы (нативный бинарник, без зависимостей;
  FS персистится) и вызывать в **CLI-режиме** из Bash: `codebase-memory-mcp cli search_graph ...`.
  MCP у поддерживаемых агентов идёт только через host-gateway; in-sandbox stdio MCP не
  предусмотрен.

## 9. Приложение A: обычный Docker (не sbx)

- Официального образа нет (#1776, open); собирается самостоятельно (release-ассеты:
  `linux-amd64-portable.tar.gz` — статический бинарь, или обычный).
- `install.sh --skip-config` / ручной `.mcp.json`; install для MCP не обязателен.
- Read-only repo: `persistence=true` падает, т.к. артефакт пишется внутрь репо (#1665; регресс
  0.10.5). В MCP `index_repository.persistence` по умолчанию **false** (`mcp.c:480`).
- Read-only cache (индекс собран на хосте, примонтирован ro): daemon пишет логи и `_config.db`
  в `${CBM_CACHE_DIR}` → MCP-сессия не стартует. Для этого принят, но **не смержен**
  `CBM_IN_PROCESS` (PR #2072, accepted 2026-09-25, `mergeable_state: blocked`): in-process
  read-only MCP без демона/локов, только analysis/scout-набор.
- Sandbox, режущий сокеты (seatbelt `(deny network*)`, строгий seccomp): AF_UNIX handshake
  ~30 c hang. Docker default seccomp и gVisor AF_UNIX разрешают. Сегодня в такой песочнице
  работает CLI-режим (без демона), но не MCP.
- Права/uid: кэш и runtime owner-private (0700) с проверкой ancestry/владельца (#1687 WSL,
  #1717 symlink); userns-remap починен (#2103); Windows-контейнеры — открытые #1533/#2023.
- Devcontainer «CBM на хосте — агент в контейнере»: #822 open, официального path mapping нет;
  воркэраунд — передавать host-path.

## 10. Приложение B: контейнер-релевантные env CBM

| Переменная | Назначение |
|---|---|
| `CBM_CACHE_DIR` | кэш (индексы, `_config.db`, логи); дефолт `~/.cache/codebase-memory-mcp` |
| `CBM_RUNTIME_DIR` | rendezvous демона (`cbm-daemon-<uid>`); дефолт `/tmp` (Linux) |
| `CBM_ALLOWED_ROOT` | confine `index_repository` (sandbox boundary) |
| `CBM_WORKERS` | число воркеров индексации (в контейнерах sysconf видит host CPU) |
| `CBM_MEM_BUDGET_MB` | pin бюджета памяти (cgroup-лимит/headroom) |
| `CBM_LOG_LEVEL`, `CBM_DIAGNOSTICS` | логи/диагностика |

## 11. Приложение C: ключевые issues/PR

| Ссылка | Суть | Статус |
|---|---|---|
| #659 | canonical_root для linked worktree/subdir | fixed, тест |
| #822 | devcontainer: host/container path mismatch | open |
| #937 | dirty-state signature watcher'а | fixed |
| #1665 | read-only repo + `persistence=true` fail | open |
| #1776 | запрос официального Docker-образа | open |
| #1687 / #1717 | permission-проверки (WSL symlink/ancestry) | open |
| #2103 | userns-remap: overflow uid как ancestor owner | fixed |
| #1533 / #2023 | Windows app-container ACL | open |
| PR #2072 | `CBM_IN_PROCESS` read-only MCP без демона | accepted, не смержен (blocked) |

## 12. Источники

- Репозиторий CBM: `src/semantic/semantic.{h,c}`, `src/pipeline/pass_semantic_edges.c`,
  `src/store/store.{h,c}`, `src/mcp/mcp.c`, `src/git/git_context.{h,c}`,
  `internal/cbm/sqlite_writer.c`, `src/watcher/watcher.c`, `vendored/nomic/`,
  `scripts/extract_nomic_vectors.py`, `docs/CONFIGURATION.md`, `README.md`.
- Docker Sandboxes: `docs.docker.com/ai/sandboxes/` (architecture, mcp-gateway, reference/cli/sbx).
