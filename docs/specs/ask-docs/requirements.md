# ask_docs MVP — requirements

Дата: 2026-11-24. Следующий вертикальный этап #128. New crates: none.

- FR-1: отдельный read-only MCP tool `ask_docs`, текущий bound project,
  обязательные question и model (`<provider>/<model>`); явный вызов разрешает
  отправку вопроса и прочитанных документов выбранному remote provider.
- FR-2: Rig 0.45 Chat Completions и существующий OpenRouter provider/credentials;
  модель независима от embeddings/rerank. Не создавать runtime или registry.
- FR-3: модель вызывает search_docs/get_doc/read_neighbors; существующий поиск и
  root boundaries сохраняются. Произвольные tools, запись и рекурсия запрещены.
- FR-4: ответ завершается через finish_answer с answer, citations (source IDs),
  insufficient_evidence. Источники создаёт host из реально возвращённого чтения;
  неизвестные IDs отвергаются. Это проверка происхождения, не истинности текста.
- FR-5: единый deadline 120 s, cancellation, максимум 6 turns / 16 tool calls,
  4096 output tokens/turn, 128 KiB accumulated context, 32 KiB tool result,
  256 KiB provider response. Limits принадлежат AskLimits; запрос может уменьшить
  max_turns. Budget stop возвращает stop_reason без выдуманного частичного ответа.
- FR-6: общие admission/CPU/HTTP owners #81, bounded response consumption;
  генеративные запросы single-shot (повтор после сетевой ошибки не автоматический).
  Session disconnect/cancel останавливает запрос. Website profile запрещает ask.
- FR-7: default build сохраняет offline boundary, feature-off выдаёт admission.
  Нет новых dependencies, secrets/transcripts в telemetry. Provider errors не
  возвращают сырое тело. Live optional, только explicit key/model.

SC-1: deterministic fixture tests покрывают цикл search/read/answer, несколько
calls, bad tools/citations, no evidence, limits, cancellation и generation errors.
SC-2: fmt/clippy/tests; native docs/linux/macos/windows CI в PR.
SC-3: одна ограниченная live-попытка на указанной пользователем free-модели;
при provider blocker зафиксировать факт без автоматических повторов.
