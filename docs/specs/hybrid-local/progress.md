# Progress: hybrid-local (итерация 4)

Ledger: одна запись на таск (что сделано, чем проверено, какие minor отложены).

- Spec opened (2026-10-23). ADR-17 записан в product design: провайдер
  `fastembed` за фичей `hybrid-local`; ORT ставится `docsbase connect fastembed`
  (runtime download, `ort-load-dynamic`), модели — только явным
  `docsbase models fetch` с pinned revision; `[providers.*]` — только в
  глобальном конфиге; `models use` пишет проект по умолчанию, `--global` —
  дефолты для новых проектов; дефолтных моделей и весов в бинаре нет.
  Version pins под 14-дневным cooldown: `fastembed = "=7.1.1"`,
  `ort = "=2.0.0-rc.13"` (пересмотреть после 2026-11-06).
  Status: T1 next.
