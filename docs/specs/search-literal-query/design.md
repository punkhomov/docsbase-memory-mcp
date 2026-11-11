# Реализация explicit query mode

Spec: requirements.md. Base main528369e, machine execution по автономному grant.

## Компоненты и поток

1. Новый src/index/query.rs: QueryMode {Syntax,Literal}, owner Default/Display/
   FromStr/serde spelling и shared boundary parse_argument(None/null→Syntax,
   wrong type/value→bounded Protocol message). Enum лёгкий, без config/global
   state. Reexport index::QueryMode. Parser helper принимает полный mode.
2. IndexHandle и ReadIndex сохраняют search() как syntax compatibility wrapper,
   добавляют search_with_mode(). Общий search_reader выбирает parser.parse_query
   только для Syntax. Literal использует fieldless UserInputLiteral для каждой
   split_whitespace лексемы, Some(Should) clauses, no prefix/slop; public
   build_query_from_user_input_ast сохраняет tokenizer/field expansion/boosts.
   Несколько анализированных токенов лексемы — existing phrase semantics.
   Нулевой term inventory literal при реальном search — Query. Не меняем
   existing empty/limit0 behavior. Никакого schema/tokenizer rebuild.
3. daemon SearchArgs содержит полный QueryMode. parse_search_args использует
   общий owner, lexical call передаёт mode; provider/render inputs остаются raw.
4. protocol добавляет fixed LITERAL_QUERY_CAPABILITY. Hello advertises обе cap.
   Shared owner validates query_mode на supported sync/async helper boundaries;
   literal requires cap before dispatch. Async search по-прежнему требует scoped
   cap/operation. Generic raw call(Request) остаётся caller-owned negotiation.
   Blocking Client хранит Hello capabilities; failure/Hello reset не оставляет
   stale successful capability. Старые syntax helpers/legacy behavior сохранены.
5. CLI parser добавляет --query-mode с owner default. Public Options не получает
   новый обязательный field: новые mode-aware run entry points передают mode в
   общий existing workflow, old run wrappers явно сохраняют Syntax. Clock,
   Config snapshot, original operation и initial-connect-only fallback прежние.
   MCP schema default/spellings от owner, корректная tags description.
6. ExternalOptions получает mode/default Syntax; report provenance mode.
   Existing frozen Runtime helper сохраняет default, mode-aware variant нужен
   external execution. Provider input/replay identities не переписываются.
   Harness код сначала проходит TDD/review и отдельный local commit, затем
   измерения запускаются на чистом pinned коде в обоих режимах. Новые reports
   только в разрешённом search-benchmark/reports, старые файлы не overwrite.

## Инварианты и зависимости

Нет нового crate: Tantivy0.26.2 reexports query_grammar at lib.rs174 и имеет
build_query_from_user_input_ast at query_parser.rs359. Occur::Should относится
к той же grammar; delimiter не включает query-language parsing. Все четыре
existing поля text/title/heading_path/identifiers и boosts1/2/1.5/2.5 сохранены.
Literal query не является whole-sentence phrase. Синтаксические кавычки не
приобретают phrase meaning; punctuation tokenization идёт через старые analyzers.

Mode/spellings/default/capability — fixed v1 semantic identity X01, не knob для
cache/timeout. Owner default один; downstream index/provider не дополняет config.
Не вводим input length/backoff/overscan/default limits без component policy.
Original request budget продолжает охватывать CPU/transport; sync parsing
кооперативно проверяется existing phase boundaries, hard preemption не обещаем.

## Ошибки и совместимость

Wrong mode — Protocol; bad Syntax/no analyzed Literal — Query. Connected old-cap
refusal не вызывает CLI direct fallback/MCP extra retry. Literal capability не
меняет PROTOCOL_VERSION1 или default arrays. Старые peers игнорируют additive
Hello fields; новый literal нельзя отправить им молча. Generic raw transport
остаётся низкоуровневым, caller обязан вести negotiation. Default syntax с
older scoped daemon допустим. No-term validation не выполняется в limit0 fast
path, поскольку он не исполняет query; field types/mode остаются валидированы.

## Проверка и альтернативы

Реальные Tantivy indexes и product tools проверяют7строк и operator cases;
публичные old/new APIs, native fake peers/cap/no-dispatch и CLI local-index
refusal проверяют seams. Existing real MCP tags test покрывает #111 parsing;
не пишем тест, зеркалящий одну description string. Fake providers ловят raw query.
External report pair сохраняет inputs/denominator/errors/provenance/spend0.

Отвергнуты: смена default (compat break), lenient query parsing (operators
остаются), escaping посредством ad-hoc replacements (хрупкая grammar), quoted
whole sentence (phrase-only), новый crate, HTTP/Node facade и tuning SciFact.

Риск: ошибочная propagation mode или quiet ignore older daemon. Mitigation:
typed owner, explicit mode-aware APIs, capability before dispatch, actual
connected old-peer refusal без fallback на valid direct index. JSON schema
новое optional поле и enum, поведение default не меняется.

### Delta 2026-11-11 — LQ1 ordinary-word equivalence

Literal AST устраняет повторные идентичные raw whitespace-лексемы, сохраняя
первый порядок, как существующая grammar устраняет одинаковые clauses. Не
нормализуем raw words сверх existing analyzer и не меняем query для providers.
Обычные запросы без operators должны сохранять scores, в том числе повторные
слова; отдельный public retrieval regression сравнивает результаты режимов.
