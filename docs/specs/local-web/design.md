# Локальная Web-панель — дизайн и ADR-WEB-1

2026-11-11. База: main 4811720. Требования: requirements.md.

Web — отдельный процесс; единственный доступ к проектам через AsyncClient.
Новая capability `panel-read-v1` разрешает read-only bind по ID без autо-index,
backfill и создания providers. Существующий RegisterUnbound не подходит: он
может регистрировать/индексировать cwd. Новый bind также удерживает daemon lease.
Конфигурация — DTO с разрешёнными полями; provenance неизвестного snapshot null.

Ограниченный пул разделён на search и control; соединение сериализовано и
привязано к ID. Всего фиксированное число slots, очереди ограничены отдельными
semaphores. Незавершённое соединение уничтожается. SearchOperation начинается
до очереди, policy берётся у демона. Дисконнект HTTP должен отменять handler;
проверка реальным TCP обязательна. Daemon scoped search учитывает physical work.

SettingsSpec WebSpec владеет дефолтами. Источник — отдельный web.toml в config
root + CLI port; изменения действуют после перезапуска, индекс не инвалидируется.

## Dependencies + rationale

Все новые зависимости optional через web; cli-full включает web. Lean без HTTP.
- axum 0.8: HTTP routing/body caps, Tokio integration.
- getrandom 0.3: OS entropy для одноразового bootstrap и session.
- ts-rs 11: Rust → TypeScript DTO, проверка drift.
Frontend: Svelte 5, TypeScript 5, Vite 6, svelte-check, Playwright; lockfile.
Конкретные versions фиксируются lockfiles, MSRV проверяется 1.88.

Ресурсы build.rs читает только из web/dist, генерирует include_bytes! allowlist.
Никаких запусков npm из Cargo. Bundle собирается в каждой native job.
CSP self-only, никаких HTML renders из документации, no-store API. Cookie
уникальна на запуск, HttpOnly/SameSite=Strict, Host/Origin проверяются.

Идентификаторы chunk_id сохраняют числовой wire-контракт существующего поиска.
UI не использует их как ключи/адреса: JSON JavaScript может округлять u64 свыше 2^53.
Статус ANN использует общую с CLI классификацию stale/disabled/loading.
