# Flaky daemon/CLI tests — описание проблемы

Дата: 2026-10-09. Статус: pre-existing (v1-тесты демона), не связаны с
search-quality. Фиксы — отдельной веткой/PR, не смешивать с #24.

CI периодически падает на Linux/Windows; перезапуск failed jobs — зелёный.
Ниже четыре подтверждённых флейка с логами CI, root cause и вариантами фикса.

## 1. `mcp_registry::second_sync_start_returns_current`

- **Где:** `tests/mcp_registry.rs:99` (`Env::index_project` → `.expect("index_project")`,
  хелпер на `:94`).
- **Симптом:** `Transport { message: "Resource temporarily unavailable (os error 11)" }`.
- **Наблюдения:** runs `37818545405` (08.10 17:45), `37899633839` (09.10 07:36),
  `37900243501` (09.10 07:42), `38404323321` (13.10 13:25, job `115319812507`);
  jobs `113453330685`, `113718981126`, `113720846683`.
- **Root cause:** `Client::connect` ставит `IO_TIMEOUT = 2s`
  (`src/ipc/client.rs:13`, `:34`); на Linux истечение `SO_RCVTIMEO` возвращает
  `EAGAIN`/`WouldBlock` (os error 11), а `io_error` (`src/ipc/client.rs:203`)
  заворачивает его в общий `Transport`. `index_project` (40 доков × 200 строк) на
  загруженном раннере может считаться дольше 2 с → ложный «transport failure».
- **Почему флейк:** зависит от загрузки раннера; тесты в одном бинаре идут
  параллельно, каждый поднимает демона.
- **Варианты фикса:**
  1. тест: в `Env::index_project` вызвать
     `client.set_read_timeout(Some(Duration::from_secs(30)))` — API для этого уже
     существует (`src/ipc/client.rs:46`, «long tools need longer»);
  2. продукт: в `io_error` различать `WouldBlock`/`TimedOut` и отдавать внятный
     `Error::transport("timed out ...")` (сейчас EAGAIN вводит в заблуждение);
  3. опционально: отдельные таймауты для long-tools в CLI.
- **Acceptance:** тест стабилен под нагрузкой; EAGAIN не трактуется как ошибка
  транспорта без пояснения.
- **Статус:** исправлено — тест использует production-таймаут `LONG_TIMEOUT`
  (600 с) для `index_project`; `io_error` различает `WouldBlock`/`TimedOut` и
  отдаёт «timed out waiting for the daemon».

## 2. `lifecycle::connection_flood_is_refused_and_daemon_survives`

- **Где:** `tests/lifecycle.rs:213` (assert «over-cap connection must be shed»).
- **Симптом:** вместо shed приходит ответ
  `{"kind":"Error", ... "build \"flood\" != \"docsbase 0.1.0-alpha.3\""}`.
- **Наблюдения:** runs `37647565450` (07.10 15:56), `37915744194` (09.10 10:12),
  rerun `37943803487` (09.10 14:26); jobs `112882211941`, `113771355281`,
  `113864696772`.
- **Root cause:** тест заполняет cap «сырыми» соединениями без hello
  (`tests/lifecycle.rs:181-187`), затем `sleep(500ms)` «дать accept-loop потребить
  слоты» (`:195`) и открывает extra с `build_id: "flood"`, ожидая, что демон
  сбросит соединение до обработки hello. Под нагрузкой 500 мс не хватает: extra
  успевает обработаться → срабатывает build-check → ответ вместо shed.
- **Варианты фикса:**
  1. детерминированно дождаться заполнения cap: handshake каждого held-соединения
     (тогда регистрация гарантирована) либо `wait_until` по наблюдаемому признаку;
  2. если cap считается по accepted-сокетам, а не по сессиям — зафиксировать это
     контрактом и дожидаться accepted == cap через тест-хук/лог.
- **Acceptance:** тест не зависит от таймингов; shed гарантированно происходит до
  чтения hello.
- **Статус:** исправлено — retry-проба до фактического насыщения cap (вместо
  фиксированного `sleep(500ms)`); shed наблюдаем и без гонки.

## 3. `error_surface::mcp_error_codes_stable` (Windows)

- **Где:** `tests/error_surface.rs:82`, `docsbase index` CLI при живом демоне.
- **Симптом:** `error: internal error: sqlite ...registry.db: database is locked:
  database is locked: Error code 5`.
- **Наблюдения:** run `37819989463` (08.10 17:53, job `113458241238`), rerun —
  зелёный.
- **Root cause:** конкуренция SQLite (CLI-индекс и демон) на Windows.
  `busy_timeout(5s)` установлен (`src/store/mod.rs:38`,
  `src/store/migrations.rs:102`), но либо лок держится дольше, либо конкретный путь
  открытия registry идёт в другом режиме (не подтверждено).
- **Варианты фикса:** подтвердить WAL + `busy_timeout` на всех путях открытия
  registry; при необходимости увеличить таймаут; стресс-тест CLI+daemon на Windows.
- **Acceptance:** конкурентный `index` при живом демоне на Windows не падает по
  lock.
- **Статус:** смягчено — registry `busy_timeout` 5 с → 30 с (`src/store/mod.rs`);
  Windows-репро не было, стабильность подтвердит CI.

## 4. `mcp_registry::watcher_refreshes_search` (Windows)

- **Где:** `tests/mcp_registry.rs:617` («watcher did not refresh search»), тест
  пишет файл и до 6 с поллит `search_docs`.
- **Симптом:** refresh не наступает; в логе демона:
  `warning: watcher batch for project 1: internal error: tantivy: Failed to open
  file for write: IoError { io_error: Os { code: 5, kind: PermissionDenied },
  filepath: "...fieldnorm" }`.
- **Наблюдения:** run `38404323321` (13.10 13:25, job `115319812833`) — первый
  зафиксированный случай; воспроизводимость не подтверждена.
- **Root cause (гипотеза, не подтверждено):** семантика разделения файлов на
  Windows — инкрементальная запись/мердж tantivy пытается открыть/пересоздать
  `.fieldnorm` сегмента, который держит открытым читатель (searcher) без
  `FILE_SHARE_DELETE`/`FILE_SHARE_WRITE`; на Linux это не проявляется. Возможен
  вклад антивируса/индексатора раннера.
- **Варианты фикса:** воспроизвести локально на Windows (стресс: watcher-правка
  при активном поиске); проверить жизненный цикл reader/writer в watcher-пути
  (reload/commit порядок), при необходимости — retry с backoff на
  `PermissionDenied` при открытии файлов сегмента или managed-directory с
  корректным sharing.
- **Acceptance:** watcher-правка при параллельном поиске на Windows стабильно
  становится видимой в пределах дедлайна теста.
- **Статус:** не исправлено — нужен Windows-репро (см. «Что дальше»).

## Что дальше

- Флейки 1–2 исправлены детерминированно (тест-сайд + понятное сообщение
  таймаута).
- Флейк 3 смягчён (busy_timeout 30 с); при повторе — воспроизводить на Windows.
- Флейк 4 ждёт Windows-репро (стресс: watcher-правка при активном поиске).
- 2026-10-13: флейки 1 и 4 воспроизвелись одновременно на docs-only PR #26
  (linux job `115319812507`, windows job `115319812833`) — rerun обычно зелёный.
