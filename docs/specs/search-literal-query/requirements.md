# Явный literal-режим поискового запроса (#87, #111)

## Проблема и область

На main528369e обычный текст передаётся QueryParser::parse_query. Семь из300
SciFact test запросов дают Error::Query из-за апострофов, двоеточия, скобок и
знаков. Это сохраняется в историческом отчёте; запросы не переписываем и ошибки
из denominator не удаляем. Нужен явный путь естественного текста с сохранением
действующего advanced syntax. В MCP tags ещё неверно описаны только inline-теги.

Потребители: CLI, MCP, текущие Rust IndexHandle/ReadIndex API и offline external
measurement. HTTP/Node/server mode, stable facade #80, изменение scoring,
фильтрации #67, дедупа #68 и tokenizer/index format вне области.

## Контракт и пользовательские сценарии

Решение в рамках автономной очереди: additive query_mode=syntax|literal.
Отсутствие/null означает syntax ради совместимости; CLI --query-mode literal
выбирает literal. Старый search(query,limit) остаётся syntax. Literal означает
текст без языка запросов, а не обязательную фразу всей строки: whitespace-лексемы
объединяются OR, внутри каждой применяется существующий field tokenizer;
несколько токенов одной лексемы сохраняют phrase/position semantics tokenizer.
Кавычки, field selectors, boolean/range/prefix/boost знаки не являются операторами.

- P1: исходные семь SciFact строк выполняются literal через настоящее ядро и
  имеют пригодный поиск; исходные строки и provider inputs сохраняются.
- P2: прежние syntax-запросы/фразы/field clauses и typed parse errors работают
  без изменения default. Клиент может явно выбрать любой режим.
- P3: старый daemon не должен молча проигнорировать literal; клиент получает
  Protocol с указанием обновить/restart daemon до отправки такого поиска.
- P4: новый full SciFact report сохраняет все300 positive-qrel cases и показывает
  mode/error counts, а прежние отчёты/fixtures остаются неизменными.

## Functional requirements

- LQ-FR-1: QueryMode владеет двумя wire spellings и default Syntax. Parser
  boundary и schema/CLI используют этого owner; consumers получают полный
  typed mode. Неверный тип/value query_mode — Protocol до provider work.
  Это versioned request default/compatibility, fixed exception X01, не новый
  deployment budget или пропущенное значение конфига. Нового config knob нет.
- LQ-FR-2: literal строит AST из fieldless literals непосредственно через public
  Tantivy AST API, без parse_query/string escaping/lenient query-language fallback.
  OR между whitespace-лексемами; prefix=false/slop0. Field boosts, analyzers,
  penalty/top-k и output shape неизменны. При limit>0 literal без searchable
  terms даёт Query до providers; прежний limit0 fast path сохраняется.
- LQ-FR-3: IndexHandle/ReadIndex добавляют search_with_mode(query,limit,mode).
  Старые search(query,limit) сохраняют syntax. Сохраняем public CLI Options и
  старые run APIs через явные compatibility wrappers; новый CLI путь передаёт
  mode, не перезапускает исходный clock/config/operation на fallback.
- LQ-FR-4: daemon advertises literal-query-v1 дополнительно к search-context-v1,
  PROTOCOL_VERSION остаётся1. Поддерживаемые helpers sync/async IPC проверяют
  query_mode и capability до literal dispatch; syntax не требует новой cap.
  Async scoped search сохраняет обязательную search-context-v1. Raw generic
  frame APIs оставляют negotiation вызывающей стороне; это явно документируем.
  Никакого downgrade literal→syntax или нового retry/direct fallback.
- LQ-FR-5: MCP schema/CLI docs явно различают режимы; default и spellings от
  QueryMode owner. Tags description поддерживает inline и block lists, AND и
  отсутствие тегов без frontmatter (#111). Parsing/indexing tags не меняются.
- LQ-FR-6: embedding/rerank получают исходный query без escaping/переписывания.
  Mode меняет lexical interpretation, не model input; existing cache/singleflight
  identities для тех же provider inputs остаются валидны.
- LQ-FR-7: external harness принимает явный mode, default syntax, записывает mode
  в report provenance. Новый измеренный Syntax/Literal pair на всех300 queries,
  те же raw input hashes и IDs; ошибки остаются явными cases/misses. Historical
  reports не перезаписываются. Mode/engine/harness/profile/input hashes честные.
  Корпус не коммитится, лицензия/источник/время загрузки указаны, spend0.

## Non-functional requirements

- LQ-NFR-1: Rust1.88/edition2024; cli-full на Linux/macOS/Windows, lean
  compatibility. Новых crates/unsafe/network default нет. Используем public
  reexport tantivy::query_grammar из locked Tantivy0.26.2.
- LQ-NFR-2: ranking weights, tokenizer/version7, windows, budgets, frozen113
  inputs и baseline не меняются. Нового persistent state/invalidation нет.
- LQ-NFR-3: тесты проверяют реальные retrieval/caller boundaries, не форму AST;
  без sleeps, host SLA и baseline regeneration. Сохраняем #66 original scope,
  cancellation/physical accounting, old-client behavior и legacy half-close.

## Success criteria

- LQ-SC-1: обе index API, все7 исходных строк literal, syntax valid/invalid,
  apostrophes/quotes/colon/brackets/signs/operators и no-term input покрыты.
- LQ-SC-2: daemon/direct CLI/MCP и sync/async helpers передают mode; incapable
  old peer не получает literal search, valid local index не скрывает отказ.
- LQ-SC-3: fake providers подтверждают исходный query; default golden/offline
  и existing advanced syntax/response contracts проходят без fixture edits.
- LQ-SC-4: reports по Syntax и Literal содержат300cases; Syntax сохраняет явные
  исходные7 parse errors, Literal имеет0 parse errors, без удаления запросов.
  Все исходные query/document/qrel IDs и raw hashes сопоставлены с old report.
- LQ-SC-5: fresh review каждой задачи, whole-feature final most-capable review,
  четыре required checks на final head до ready/normal squash merge.

## Предположения и challenge

Смена default на literal ломала бы намеренные operator queries: сохраняем syntax.
Lenient parser всё ещё трактует operators: строим literals AST, не исправляем
ошибочный запрос на лету. Quoting всей строки создало бы phrase-only поиск:
явно OR между лексемами. Старый daemon игнорирует неизвестные args: новая cap
обязательна для literal. Query mode не входит в provider input, поэтому не
дробим paid cache key без изменения реального input. Request default — semantic
compatibility X01, не новый неуправляемый operational setting.

Raw dataset отсутствовал локально, canonical host blocked CONNECT403. Найден
публичный pinned mirror, все3 BLAKE3 точно совпали с historical report:5183docs,
1109queries,300positive test IDs. Proof в scratch/issue-87/dataset-verification.json.
Среда/сетевая политика не менялись; paid key для этой задачи не требуется.
