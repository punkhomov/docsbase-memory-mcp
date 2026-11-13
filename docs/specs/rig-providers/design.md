# Design и ADR-RIG1 — кодеки Rig, транспорт docsbase

## Решение, 2026-11-12

Принято для первого этапа #128. Используем rig-core =0.45.0, default-features=false,
только в hybrid-remote. Дата публикации 2026-10-19T17:49:32Z из crates.io sparse
index; 0.46.0 опубликован 2026-10-31 и ещё не прошёл 14 дней. MSRV 1.97.1
(объявлен самим Rig), обновляется во всех CI workflows. Nightly остаётся
resolver toolchain для cooldown; Rust 1.100.0 manifest 2026-11-12 ещё даёт 404.

### Dependencies + rationale

| Crate | Назначение | Features |
|---|---|---|
| rig-core =0.45.0 | OpenAI-compatible embedding/rerank Wire encode/decode | default-features=false, optional hybrid-remote |

Rig 0.45 разделяет чистый Wire и async driver/transport. Используем Wire::encode
и wire::decode_reply поверх существующего синхронного Transport. Это реальный
рабочий путь, не fallback или opt-in эксперимент. ReqwestTransport, retries,
лимиты batch и наши Embedder/Reranker контракты остаются владельцами исполнения.
Rig не получает собственный reqwest, agent runtime или vector store.

## Границы и совместимость

| Контракт docsbase | Решение |
|---|---|
| Provider registry, config/readiness | Прежняя единственная фабрика |
| OpenRouter payload/response | Кодеки Rig и небольшие compatibility поправки |
| query/document | Явно добавляем input_type: search_query/search_document |
| embedding index | Проверяем и упорядочиваем raw data до Rig decode |
| f64 в Rig / f32 в cache | Проверяем representability, затем конвертируем |
| normalization/cache key | Сохраняем прежний f32 L2 и identity |
| rerank | Включаем /rerank в локальной копии dialect, сохраняем top_n |
| credentials/headers | Прежний secret owner; без logging payload/ключа |
| retries/deadline | Только прежний transport, ограничения #81 не решены |
| FastEmbed | Прежний локальный адаптер через общий Provider/Stages |

## Проверенные подводные камни

1. OPENROUTER dialect 0.45 наследует unsupported rerank: нельзя просто вызвать
   стандартный client.rerank. Меняем path в копии dialect на уже используемый
   /rerank, не форкаем SDK.
2. EmbeddingsDecoder игнорирует data.index и сохраняет порядок ответа. Без
   проверки до decode перестановка ответа загрязнит cache чужими векторами.
   Missing index у Rig становится 0; distinction missing/present проверяем раньше.
3. Embedding request не имеет query/document input_type; добавляем его в
   сформированный JSON. Это семантика модели, не необязательная оптимизация.
4. Rig хранит числа в f64, docsbase — f32. Нельзя допускать infinity после cast.
5. rig-fastembed 0.45 зависит от fastembed ^4.5, здесь =7.1.1 и ORT rc.13 с
   собственным load-dynamic provisioning. Его прямое подключение не заменяет
   адаптер и может конфликтовать по native links; не добавляем/не понижаем.
6. Синхронные traits и текущие retries могут долго занимать blocking worker.
   Кодеки позволяют начать миграцию, но не заменяют async/budgets работу #81.

7. Записанные rerank fixtures содержат document: {text: ...}, а Rig ожидает
   Option<String>. Это поле не входит в результат docsbase; удаляем echoed
   document/text перед decode, сохраняя index/score как единственный источник.
8. Cargo feature unification включает serde_json/preserve_order и float_roundtrip
   во всей remote сборке. Golden citations сортируют только ключи JSON objects,
   сохраняя порядок массивов и прежние snapshots. Все четыре упавших snapshot
   были равны прежним после JSON parsing; ranking/content не менялись.

## Альтернативы

- Полностью заменить client на Rig async driver сейчас: объединяет смену
  SDK с execution refactor и рискует вложенными runtimes/retries. Отложено #81.
- Отдельный режим rig-* рядом с openrouter: дублирует конфиг и миграцию. Отклонено.
- Заменить весь retrieval на Rig RAG: вне задачи, наши индексы/cache сохраняются.

## Границы первого этапа

Connect/model probes остаются прежними single-shot проверками: они не являются
вторым runtime режимом и не заменены на Rig verify, чтобы не менять admission
и платность запросов. Metadata usage/model/object и echoed documents раньше
игнорировались; проекция перед decode сохраняет это поведение. Usage для
будущего harness потребует отдельного явного контракта.

Новый Clippy 1.97 требует механической миграции Duration constructors,
let-chains, map_or и test assertions. Эти изменения сохраняют поведение,
проверяются общим suite и не ослабляют lint policy.

## Следующее

После этого этапа: общий async model execution, capability-aware provider
profiles, генерация и custom Inception Chat Completions; bounded ask_docs.
Rig::Local может стать адаптером для нашего FastEmbed, если появится потребитель
его async интерфейса. Обёртка только ради унификации типов сейчас не нужна.

Источники: опубликованные исходники rig-core 0.45.0 (wire.rs, providers/openai/
wire/{dialects,modality}.rs), sparse index rig-core и rig-fastembed;
https://github.com/0xPlaygrounds/rig, https://rig.rs/docs/.
