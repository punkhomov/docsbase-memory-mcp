# Search Telemetry — Progress Ledger

Ledger исполнения `docs/specs/search-telemetry/tasks.md`. Одна задача — одна
запись при коммите. Ruling фиксируют принятые решения; minor — что отложено.

- Spec rev 0.2 (2026-10-31): первый драфт заблокирован ревью (3×P1) и исправлен:
  конверт заменён на opt-in `explain` (конституция: ломающее — только в мажоре),
  `cost_usd` вынесен в `search-cost` (нет канала usage в проде), `status.ann`
  получил хранилище `AnnOutcome` (у `status` нет доступа к `DenseCache`).
  Ruling: форма ответа по умолчанию не меняется ни в одном из вариантов спека.

