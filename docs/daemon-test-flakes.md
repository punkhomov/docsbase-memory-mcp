# Flaky daemon/CLI tests — описание проблемы

Дата: 2026-10-09. Статус: pre-existing (v1-тесты демона), не связаны с
search-quality. Фиксы — отдельной веткой/PR, не смешивать с #24.

CI периодически падает на Linux/Windows; перезапуск failed jobs — зелёный.
Ниже шесть подтверждённых флейков с логами CI, root cause и вариантами фикса.

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
- **Root cause (раскрыт 2026-10-14, два механизма):**
  - **A — наблюдался в CI (run `38608476841` и ранние):**
    `PRAGMA journal_mode=WAL` в `Db::open` возвращает мгновенный
    `database is locked` при параллельном первичном открытии (смена
    journal-mode не покрывается busy-handler'ом). Доказательство: в логе
    ошибка несёт префикс пути `sqlite {path}: ...` — так оборачивает только
    `sql_error` (`Db::open`), тогда как ошибки транзакций идут через
    `db_error` без пути; при extended-коде 517 в логе было бы «517», а не 5
    (libsqlite3-sys печатает extended). **Исправлено:** bounded-ретрай
    (до 3 попыток, пауза 100 мс) только для `DatabaseBusy` вокруг
    `PRAGMA journal_mode=WAL`.
  - **B — класс, доказан тестом:** deferred-транзакция с read→write-upgrade
    падает `SQLITE_BUSY_SNAPSHOT` (extended 517) мгновенно, как только
    другое соединение закоммитило между read и write; `busy_timeout` его не
    покрывает. Repro: `read_then_write_tx_survives_concurrent_writer`
    (красный на Deferred с `extended_code: 517`, зелёный на Immediate).
    Продакшен-транзакции были write-first (класс в CI не наблюдался), но
    фикс закрывает его по конструкции: все пишущие транзакции — через
    `Db::begin_write` (`BEGIN IMMEDIATE`, `src/store/mod.rs`).
- **Acceptance:** конкурентный `index` при живом демоне на Windows не падает по
  lock; стресс 50/50 (run `38590476555`) и полный сьют — зелёные.
- **Механизм B — класс, доказан тестом:** deferred-транзакция с
  read→write-upgrade падает `SQLITE_BUSY_SNAPSHOT` (extended 517) мгновенно,
  как только другое соединение закоммитило между read и write;
  `busy_timeout` его не покрывает. Repro:
  `read_then_write_tx_survives_concurrent_writer` (красный на Deferred с
  `extended_code: 517`, зелёный на Immediate). Продакшен-транзакции были
  write-first (класс в CI не наблюдался), но фикс закрывает его по
  конструкции: все пишущие транзакции — через `Db::begin_write`
  (`BEGIN IMMEDIATE`, `src/store/mod.rs`).
- **Статус:** исправлено (A + B); ранее: `busy_timeout` 5→30 с и стресс 50/50
  (run `38590476555`) — оставлены.

## 4. `mcp_registry::watcher_refreshes_search` (Windows)

- **Где:** `tests/mcp_registry.rs:617` («watcher did not refresh search»), тест
  пишет файл и до 6 с поллит `search_docs`.
- **Симптом:** refresh не наступает; в логе демона:
  `warning: watcher batch for project 1: internal error: tantivy: Failed to open
  file for write: IoError { io_error: Os { code: 5, kind: PermissionDenied },
  filepath: "...fieldnorm" }`.
- **Наблюдения:** run `38404323321` (13.10 13:25, job `115319812833`) — первый
  зафиксированный случай; повтор run `39133552920` (18.10, windows job
  `117693411488`, тест `watcher_retries_transient_batch_failure`): все три
  ретрая попали в гонку создания файла удалённого сегмента
  (`FileAlreadyExists("*.del")`), батч дропнут, тест не увидел правку;
  тот же upstream-механизм (#2847), другое расширение окна (`.del`). Рerun —
  зелёный.
- **Root cause (наиболее вероятный; механизм upstream оспаривается):** сигнатура
  точно совпадает с tantivy issue #2847: merge-поток `IndexWriter` гоняется с
  commit за `.managed.json` (`MoveFileExW`), а ошибка обёрнута именем сегментного
  файла (`managed_directory.rs:287`) — отсюда `.fieldnorm` в логе. Однако в issue
  есть содержательный контраргумент (STiFLeR7, 2026-07-28,
  https://github.com/quickwit-oss/tantivy/issues/2847#issuecomment-5100620847):
  официальное репро 14× (280 коммитов) — 0 падений; `meta_wlock` уже держится
  через `save_managed_paths`, клоны делят один `RwLock` через `Arc`. Возможные
  альтернативы: AV-софт раннера, окружение, более узкое окно гонки. Доказано у
  нас: дроп батча теряет правки, а ретрай/видимость чинят симптом
  детерминированно; сериализация мержей (drop-wait) полезна независимо от
  исхода спора (на Linux там же — тихая потеря регистраций `.managed.json`).
  Upstream-фикса нет, issue открыт и трекается.
- **Фикс (наш слой):** 1) `IndexHandle` при drop вызывает
  `wait_merging_threads()` — merge-потоки завершаются до следующего commit,
  записи `.managed.json` сериализуются; 2) bounded retry батчей (3 попытки,
  100/300/900 мс) как страховка на прочие транзиенты; 3) финальный дроп виден
  в `status` (`last_error` + потерянные пути в `warnings`).
- **Acceptance:** Linux fault-injection тесты (retry и drop-видимость) —
  детерминированный гейт; Windows 50× — статистический сигнал; upstream #2847
  остаётся открытым и отслеживается.
- **Статус:** исправлено на нашем слое (defense-in-depth: drop-wait + bounded
  retry + видимость дропа); механизм upstream не подтверждён — трекаем #2847.
  Стресс: workflow `flake-stress` (workflow_dispatch, по умолчанию 50 прогонов;
  Windows — `watcher_refreshes_search`, Linux — flood-тест).

## 5. `mcp_registry::second_sync_start_returns_current` (assertion race, Linux)

- **Где:** `tests/mcp_registry.rs` — второй `sync_start` ожидал ту же активную
  job, но первая (no-op sync) успевала завершиться между вызовами, и второй
  вызов корректно создавал новую.
- **Наблюдения:** 2026-10-14, run `38585189823` (linux, дважды подряд).
- **Root cause:** гонка теста, не продукта: FR-17 («не более одной активной
  job») не нарушался — первая job была terminal.
- **Фикс:** если job_id различаются, тест дожидается terminal-состояния первой
  (через `wait_job_done`) и проверяет непересечение интервалов
  (`finished_at(первая) <= started_at(вторая)`); равенство id по-прежнему
  требует state queued/running.

## 6. `watcher::edit_becomes_searchable_within_budget` (budget misalignment, Linux)

- **Где:** `tests/watcher.rs` — e2e-бюджет 2 с накрывал debounce (1.5 с, кап
  `MAX_DEBOUNCE` 2 с) и обработку, тогда как FR-15/NFR-4 меряют их раздельно:
  flush ≤ `MAX_DEBOUNCE`, искомость ≤ 2 с уже **после** debounce.
- **Наблюдения:** run `38596192159` (14.10, релизный PR #38): «freshness took
  2.258653187s» под параллельной нагрузкой раннера.
- **Root cause:** конструкция теста, не продукт: номинал 1.5 с (debounce) +
  ~0.2 с (индексация+поиск) давал лишь ~300 мс запаса.
- **Фикс:** бюджет разбит на две фазы с отдельными ассертами — `FLUSH_BUDGET`
  (Linux: `MAX_DEBOUNCE` + 1 с CI-джиттера; macOS 5 с; Windows 15 с) и
  `POST_FLUSH_BUDGET` (Linux: NFR-4-овые 2 с; macOS 3 с; Windows 5 с).

## Что дальше

- Флейки 1–2 исправлены детерминированно (тест-сайд + понятное сообщение
  таймаута).
- Флейк 3 смягчён (busy_timeout 30 с); при повторе — воспроизводить на Windows.
- Флейк 4 исправлен на нашем слое (drop-wait + bounded retry + видимость в
  status); upstream tantivy#2847 остаётся открытым — при апгрейде tantivy
  проверить его статус.
- 2026-10-13: флейки 1 и 4 воспроизвелись одновременно на docs-only PR #26
  (linux job `115319812507`, windows job `115319812833`) — rerun обычно зелёный.
- Флейк 5 (2026-10-14, run `38585189823`): исправлен тестом — детерминированная
  проверка терминальности первой job и непересечения интервалов (секундная
  гранулярность timestamps; перекрытие внутри одной секунды не детектируется).
- Флейк 6 (2026-10-14, run `38596192159`): исправлен тестом — бюджет свежести
  разбит на flush-фазу (FR-15) и post-flush-фазу (NFR-4).
