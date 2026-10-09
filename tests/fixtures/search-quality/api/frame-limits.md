# Frame limits

NDJSON-кадр ограничен `MAX_FRAME_BYTES` (8 MiB); ответ `get_doc` резервирует
`FRAME_OVERHEAD` под JSON-обёртку.

Превышение лимита закрывает соединение с ошибкой протокола.
