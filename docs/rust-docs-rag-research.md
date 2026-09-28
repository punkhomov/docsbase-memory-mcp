# Docs RAG на Rust — исследование и план

> Статус: черновик. Дата: 2026-09-28.
> Область: собственный local-first MCP-сервер памяти по документации.
>
> **Часть решений устарела.** Актуальная спецификация — `docs/specs/docsbase-memory-mcp/requirements.md`:
> per-project индексы вместо одного общего, явная регистрация проекта (`index_project`),
> shared-слой отложен, вектора — фаза 2. Ниже — исторический контекст и сравнительный анализ.

## 1. Цель и ограничения

Построить **свою** тулзу на Rust по модели использования `codebase-memory-mcp`, но для документации:

- постоянный индекс **только `.md`** файлов (Markdown-first);
- доступ через MCP (Codex, OpenCode, другие MCP-агенты);
- один индекс, шарится между несколькими агентами;
- local-first / self-hosted, без внешних embedding API;
- hybrid retrieval (BM25 + vector), exact-identifier поиск обязателен.

### Решения, принятые по ходу обсуждения

- **PDF/DOCX не парсим вообще** — только `.md`. Это снимает главный риск (слабый парсинг PDF/DOCX в Rust) и превращает проект из «недель» в «выходные».
- **Rust** — ради одного статического бинаря, без Python-рантайма, предсказуемого idle RAM.
- **Вектора — фаза 2**, за фича-флагом. Сначала BM25 + грамотный chunking; `fastembed`/HNSW добавляем, только если бенчмарк покажет дыры.
- Collections = поле в метаданных, не отдельные индексы.

## 2. Роль каждого референса

| Проект | Стек | Retrieval | Хранилище | MCP транспорт | Источники | Scope-фильтр | Зрелость |
|---|---|---|---|---|---|---|---|
| [mcp-local-rag](https://github.com/shinpr/mcp-local-rag) | Node/TS | vector + keyword boost | LanceDB (vectors + FTS) | stdio | pdf/docx/md/txt/html | path-prefix | 405★, 683 коммита, живой, MIT |
| [leona/kb](https://github.com/leona/kb) | Go | только full-text | md-файлы + git | stdio | md | project/shared/global | 16★, 25 коммитов, молодой |
| [codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp) | C | BM25 FTS5 + vector + 11 сигналов | SQLite | stdio + daemon | код | repo/project | 45.3k★, но про код |
| [self-doc](https://github.com/AdamRussak/self-doc) | Python | vector(pgvector) + FTS | Postgres16+pgvector | streamable HTTP | crawl/upload | source | 4★, **лицензия Private — код не переиспользовать** |
| [Crawl4AI RAG](https://github.com/coleam00/mcp-crawl4ai-rag) | Python | vector+hybrid+rerank+contextual | Supabase pgvector | sse/stdio | web crawl | domain | 2.3k★, но 19 коммитов, автор называет «testbed» |
| [Docs-MCP](https://github.com/matin-g/Docs-MCP) | Python | pure vector | ChromaDB | stdio | sitemap crawl | collection | 0★, 1 коммит — мёртв |
| [Qdrant MCP](https://github.com/qdrant/mcp-server-qdrant) | Python | vector only | Qdrant | stdio/sse/streamable-http | нет (store/find) | collection_name | 1.5k★, официальный, но платформа |

Большинство — **не Rust** и **не md-only**, поэтому это источники паттернов, а не код для форка.

## 3. Детали по референсам

### 3.1. mcp-local-rag — главный ориентир по retrieval/tools/sync

- Локально всё: парсинг, эмбеддинги, storage, поиск. Без API-ключа, Docker, Python, внешней БД.
- Hybrid: векторный поиск + keyword boost (вес `RAG_HYBRID_WEIGHT`, 0.0–1.0, дефолт 0.6). Опции: `RAG_GROUPING` (relevance-gap), `RAG_MAX_DISTANCE`, `RAG_MAX_FILES`.
- **Семантический chunking** по границам тем, **Markdown code blocks остаются целыми**.
- LanceDB хранит чанки, метаданные, векторы и full-text индекс.
- MCP tools: `sync_start`, `sync_status`, `ingest_file`, `ingest_data`, `query_documents`, `read_chunk_neighbors`, `list_files`, `delete_file`, `status`.
- Sync: пропускает byte-identical, удаляет исчезнувшие; только один job в процессе, при рестарте теряется.
- Безопасность: файловые операции только внутри `BASE_DIR`/`BASE_DIRS`, symlink наружу отклоняется, **без auth, рассчитан на одного локального пользователя**.
- Внешний реранк через `RAG_RERANK_CMD` (командный шаблон, вход по stdin).
- Смена `MODEL_NAME`/`RAG_DEVICE`/`RAG_DTYPE` делает старые векторы несовместимыми.
- Дефолтная модель `Xenova/all-MiniLM-L6-v2` (англо-центричная, для мультиязычности надо менять).

### 3.2. leona/kb — модель knowledge base и multi-agent setup

- **Не vector RAG**, а централизованная markdown-KB в `~/knowledge-base/` с git-версионированием (auto-commit на каждую запись).
- Структура: `shared/`, `projects/`, `kb.yml`, `refs.yml`, `meta.yml`.
- **Ref vs Inline** — ключевая идея: большие доки агент тянет по требованию (`kb_read`/`kb_search`), маленькие инлайнятся прямо в `context.md` при старте сессии.
- `kb setup` **автоматически прописывает MCP-конфиг и `@import`** в Claude Code (`.mcp.json`), Codex (`.codex/config.toml`), OpenCode (`opencode.json`).
- 16 MCP tools: `kb_context` (auto-detect project из cwd), `kb_search` (scope project/shared/all), `kb_read` (offset/limit), `kb_write` (auto-commit), `kb_draft`, `kb_log`/`diff`/`show`/`revert`, `kb_ref_add`/`inline`, `kb_global_add` и др.
- Есть TUI-браузер.

### 3.3. codebase-memory-mcp — daemon/sharing и FTS-токенизатор

- **Один статический бинарь, ноль зависимостей** (C, не Rust).
- **Session coordination daemon**: один индекс на все агенты; первая daemon-backed сессия поднимает демон, каждая сессия регистрирует работу, последняя гасит. Общий watcher/индексация/UI.
- BM25 через **SQLite FTS5 с токенизатором `cbm_camel_split`** (camelCase / snake_case aware).
- Semantic search с вшитыми Nomic embeddings (768d int8, без API).
- 17 MCP tools, CLI-режим, auto-index на старте сессии, фоновый watcher.
- Team-shared artifact: zstd-сжатый снапшот SQLite, коммитится в репо. Сами предупреждают: коммит на каждый save раздувает историю до гигабайт.

### 3.4. self-doc — progressive disclosure и safety (только идеи, код закрыт)

- Server mode: Postgres16 + pgvector, FastMCP streamable HTTP, FastAPI ingestion, optional headless renderer, Traefik.
- Hybrid: vector + per-source-language Postgres FTS. `search_docs(query, source?, limit?)`.
- Sources в БД (crawl или upload), admin UI **только loopback**, агент предлагает source → human approval.
- `llms.txt` preference + conditional GET (`ETag`/`If-Modified-Since`) для re-crawl.
- **Progressive disclosure**: `doc-cli search` → candidate IDs + heading paths + сниппеты (~50–150 токенов); `doc-cli get <id>` → полный markdown.
- `propose_doc_source`, `upload_doc_text` (лимиты 1 MB / 200 символов title).
- **Injection quarantine** для недоверенного doc-контента.
- Embedding dim вшит в схему БД — классическая ловушка dimension mismatch.

### 3.5. Crawl4AI RAG — стратегии retrieval

- Стек: Crawl4AI + Supabase pgvector + OpenAI embeddings (по умолчанию), опционально Neo4j.
- Флаги стратегий:
  - **contextual embeddings** — обогащение эмбеддинга контекстом всего документа (LLM на чанк);
  - **hybrid search** — vector + keyword merge;
  - **agentic RAG** — извлечение code blocks (≥300 символов) с summary в отдельную таблицу + tool `search_code_examples`;
  - **reranking** — локальный cross-encoder `cross-encoder/ms-marco-MiniLM-L-6-v2` на CPU (+100–200ms);
  - **knowledge graph** — Neo4j для детекции галлюцинаций.
- Кода примера — как отдельный поисковый слой, сильная идея для dev-доков.

### 3.6. Остальные

- **Docs-MCP**: sitemap → crawl → HTML→md → chunk 1000/overlap 200 → OpenAI embeddings → ChromaDB → `search_docs`. Наивно, мёртв (1 коммит). Не ориентир.
- **Qdrant MCP**: тонкий `qdrant-store`/`qdrant-find` поверх Qdrant, FastEmbed (all-MiniLM-L6-v2), транспорт stdio/sse/streamable-http, `QDRANT_READ_ONLY`. Фишка — `TOOL_STORE_DESCRIPTION`/`TOOL_FIND_DESCRIPTION` как env: tool descriptions перенастраиваются под сценарий (например, code search). Платформа только — ingestion/chunking/watcher/hybrid пиши сам.
- **Context7**: hosted, version-specific docs, триггер `use context7`. UX-референс, не self-hosted.

## 4. Паттерны, которые берём

1. **Sync-модель**: `sync_start` → `jobId`, клиент поллит `sync_status`; пропуск byte-identical; удаление исчезнувших. Job держать persistent в SQLite (у mcp-local-rag теряется при рестарте).
2. **Progressive disclosure**: поиск отдаёт ID + heading-path + короткий сниппет (~50–150 токенов), полный markdown — вторым вызовом по ID. Главный приём экономии контекста.
3. **Chunking по heading + сохранение code fences**: breadcrumb `file > H1 > H2` как префикс и в FTS, и (позже) в embedding — дешёвая версия contextual embeddings.
4. **Изоляция roots + отклонение symlink наружу**; auth не нужен, дизайн «один локальный пользователь».
5. **Ref vs Inline + context-бюджет**: большие доки тянутся по требованию, маленькие инлайнятся в контекст при старте сессии.
6. **Auto-setup MCP-конфига** для Claude/Codex/OpenCode + `@import` — под multi-agent цель.
7. **Git-версионирование KB**: auto-commit, `log/diff/show/revert`, запись через `draft → write`.
8. **FTS-токенизатор с camelCase/snake_case split** — exact identifiers (`defineStore`, `assessment_plan_id`, `X-Request-ID`).
9. **Shared daemon**: один индекс, поднимается первой сессией, гасится последней; общий watcher.
10. **Tool descriptions как env** — дешёвая перенастройка.
11. **Отдельный code-example store** и **local cross-encoder rerank на CPU**.
12. **Source как фильтр** — везде; collections = поле метаданных.
13. **Injection quarantine** — доки как недоверенный вход.

## 5. Анти-паттерны (не копировать)

- Тяжёлый стек Postgres+pgvector+Docker — для md-only оверкилл, теряет local-first.
- OpenAI-only embeddings — убивает офлайн.
- Фиксированный chunk 1000/overlap 200 — режет код и секции.
- Коммит бинарного индекса на каждый save — раздувает git до гигабайт.
- Проекты-«testbed» и мёртвые репо как основу.

## 6. Предлагаемая архитектура (Rust, md-only)

```
.md files on disk
      │
   ingest (walk + frontmatter + heading-chunker)
      │
   SQLite (metadata + content hash)  ──┐
      │                                │
   tantivy (BM25, camel/snake tokenizer)
      │
   [фаза 2] vectors (fastembed + usearch) + RRF
      │
   MCP server (rmcp)
      │  stdio  ← локальные агенты
      │  streamable HTTP (localhost) ← общий сервис
      │
   Codex / OpenCode / others
```

### Стек

- MCP: **`rmcp`** (stdio + Streamable HTTP).
- BM25/FTS: **`tantivy`**.
- Chunking: **`comrak`** / `pulldown-cmark` (heading-дерево, code fences, frontmatter).
- Watcher/инкремент: **`notify`** + content hash в SQLite (`rusqlite`).
- CLI: **`clap`**.
- HTTP-режим: **`axum`**.
- Фаза 2: **`fastembed`** (ONNX, локально, CPU; bge/e5 + bge-reranker) + **`usearch`**/`hnsw_rs`.

### Модель чанков

- Срез по heading-дереву, не по символам.
- Breadcrumb `file > H1 > H2 > H3` префиксится в FTS-текст.
- Code fences не рвать (целиком в чанк, тег `lang`).
- Frontmatter → метаданные (source/collection/tags/version) для фильтрации.
- Инкремент: mtime + content hash; удаление stale чанков.

### MCP tools (черновик)

- `search_docs(query, collection?, limit?)`
- `get_doc(path)` / `get_chunk(id)`
- `read_neighbors(chunk_id)` (progressive disclosure)
- `list_docs(collection?)`
- `sync_start` / `sync_status`
- `status`

## 7. Бенчмарк (на md-корпусе)

Четыре типа запросов:

```text
semantic:        How should authentication tokens be refreshed?
exact id:        assessment_plan_id
error message:   Body thickness and Sheet Metal component rule thickness are different
code/API:        defineStore setup store syntax
```

Метрики: top-1 relevance, top-3 relevance, exact-identifier recall, source correctness, query latency, indexing time.

## 8. План по фазам

1. **Фаза 1 (MVP)**: walk `.md` → heading-chunker → tantivy BM25 + camel/snake tokenizer, SQLite hash-инкремент, MCP tools (поиск/чтение/neighbors/list/sync/status), stdio.
2. **Фаза 2**: streamable HTTP + shared daemon (один индекс на агентов), watcher.
3. **Фаза 3**: вектора (`fastembed`+`usearch`) + RRF + reranker за фича-флагом.
4. **Фаза 4**: auto-setup конфигов Codex/OpenCode, коллекции/namespaces, injection-quarantine.

## 9. Открытые вопросы

- Точный список crates и версии (проверить актуальные).
- Формат breadcrumb/метаданных для citations.
- Нужен ли git-версионируемый KB-слой (как leona/kb) или только retrieval.
- Нужен ли отдельный code-example store.
- Демон vs просто HTTP-сервис: насколько сложна session-coordination модель.
