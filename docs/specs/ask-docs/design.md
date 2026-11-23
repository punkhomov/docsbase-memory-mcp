# Design / ADR-ASK1

Дата: 2026-11-24. New crates: none. rig-core остаётся =0.45.0.

Request-scoped model address использует существующий Provider registry.
Provider получает generation factory; FastEmbed явно не поддерживает generation.
Persistent generation settings/profiles остаются дальнейшим #128, MVP требует
model в каждом вызове и не выбирает платную модель автоматически.

AskLimits — владелец протокольных caps этого MVP (не скрытые transport defaults).
Один SearchOperation::with_timeout на ingress; nested search использует тот же
operation, reads/codec — bounded CPU. Контекст резервируется в существующем
PayloadBudget; большой документ отвергается с предложением читать соседние chunks.

Rig Completion::prepare + Chat Wire + decode_reply сохраняют assistant messages,
call IDs и provider-native continuation/reasoning. Host не публикует reasoning.
OpenRouterClient использует общий RemoteExecution; single-shot request с меньшим
response cap. HTTP status сохраняется в sanitized error, raw body не раскрывается.

Модель получает только три read tools и finish_answer. Read results оборачиваются
source IDs; finish принимает только существующие IDs. Text response без finish не
считается подготовленным ответом. Неизвестные/невалидные вызовы получают bounded
ошибку и расходуют тот же tool/turn budget. Источники — пути/строки/хеш текста,
а не сгенерированные моделью URL. Нет обещания snapshot isolation между чтениями.

MCP transport не повторяет ask после потери соединения (возможна уже оплаченная
генерация). IPC CallTool ask использует scoped cancellation/EOF monitoring.
Frontend увеличивает только timeout ask, сохраняя существующий search protocol.

Один task ASK1: вертикальный MVP с тестами и документацией. Ревью и финальная
acceptance публикуются в progress/PR. Waiting Actions не входит в час пользователя.
