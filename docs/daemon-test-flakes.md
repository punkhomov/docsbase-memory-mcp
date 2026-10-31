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

## 4. `mcp_registry::watcher_refreshes_search` / `watcher_retries_transient_batch_failure` (Windows)

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
- **Полный механизм (разбор 2026-10-18, run `39133552920`, windows job
  `117693411488`).** Двухстадийная воронка:
  1. *Триггер:* `Access is denied (os error 5)` приходит **посреди коммита**
     (та же гонка merge-потока и commit за файлы каталога, #2847); часть
     файлов коммита уже создана, `meta.json` ещё нет.
  2. *Усилитель (наш):* tantivy открывает файлы через `create_new(true)`
     (`mmap_directory/mod.rs`), а имя `.del` = `segment.{delete_opstamp}`.
     После падения opstamp не сдвинулся, ретрай переигрывает те же удаления и
     упирается в `FileAlreadyExists` **навсегда** — все 3 попытки падают
     детерминированно, батч дропается. Именно поэтому «перезапусти и пройдёт»
     работало не всегда.
- **Фикс (наш слой, 2026-10-18):**
  1) `IndexHandle::commit()` при любой ошибке коммита пишет `REBUILD_MARKER`:
     следующий `open_or_create` пересоздаёт каталог (`create_fresh`) и джоба
     перестраивает индекс из `SQLite` (`was_recreated` → полный ребак,
     T27) — отравленный `.del`/`.managed.json` больше не может залипнуть;
  2) `create_fresh` делает bounded-retry `remove_dir_all` (20×100 мс) —
     на Windows ридеры с mmap успевают отпустить файлы;
  3) в окне перестройки чтение отвергает помеченный индекс явной ошибкой
     «needs to be rebuilt» (как и раньше, T27) — окно короткое; демон-тесты
     поллят поиск через это окно (tolerant poll), а не падают на первом
     отвергнутом чтении; `watcher_refreshes_search` / `watcher_retries_…` /
     `watcher_recovers_…` обновлены соответственно;
  4) drop-wait merge-потоков и bounded retry батчей (3 попытки, 100/300/900 мс)
     остаются как было.
- **Детерминированные тесты фикса:** `tests/index_recovery.rs` (lib-уровень:
  инжект `DOCSBASE_INDEX_FAULT=commit:1` → маркер → следующий запуск лечит и
  ищет документ) и `mcp_registry::watcher_recovers_from_a_commit_failure`
  (демон: `DOCSBASE_WATCH_FAULT=commit:1` → батч-ретрай лечит через
  пересоздание). Оба — в обычном CI.
- **Acceptance:** Linux fault-injection тесты (retry и drop-видимость) —
  детерминированный гейт; Windows 50× — статистический сигнал; upstream #2847
  остаётся открытым и отслеживается.
- **Статус:** исправлено на нашем слое (defense-in-depth: drop-wait + bounded
  retry + видимость дропа + **self-heal через маркер при сбое коммита**);
  механизм upstream не подтверждён — трекаем #2847. Стресс: workflow
  `flake-stress` (workflow_dispatch, по умолчанию 50 прогонов; Windows —
  `watcher_refreshes_search` и `watcher_retries_transient_batch_failure`,
  Linux — flood-тест). Стресс-прогон 2026-10-18 (run `39137190895`): все 4
  таргета ×50 зелёные.

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

## 7. `config_runtime::watcher_uses_reopened_config` (таймаут на Windows)

- **Где:** `tests/config_runtime.rs` — два ожидания по 15 с: «v1-вотчер
  проиндексировал `bbb/seed.md`» и «после смены `ignores` появился
  `aaa/a.md`».
- **Наблюдения:** 2026-10-29, run `40882092301` на main (job
  `123475988652`, windows): `panicked at tests\config_runtime.rs:366:
  watcher must index newly unignored aaa/**`, 7 passed / 1 failed после 50 с.
- **Root cause:** не продукт, а бюджет. `ReadDirectoryChangesW` копит события
  и отдаёт батч только при заполнении буфера или коротком простое; на общем
  раннере debug-сборка идёт в 2–3 раза медленнее dev-хоста, и перезагрузка
  конфига на хендшейке добавляет ещё один round-trip. 15 с локально — это
  ~4–5 с на раннере при удачном стечении.
- **Фикс:** общий `watcher_deadline()` в `tests/config_runtime.rs` — 60 с на
  Windows, 20 с на Unix. Граница гейтит только путь ошибки: успешный прогон
  выходит из цикла сразу, как только запись проиндексирована, поэтому
  зелёные прогоны не замедляются.

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
- Флейк 7 (2026-10-29, run `40882092301`): исправлен тестом — платформенный
  дедлайн ожидания батча вотчера (`watcher_deadline()`), см. `docs/ci.md`
  про host-relative бюджеты.

## Flake #8 — `cold_start_loads_persisted_ann_without_vectors_db` (Windows)

**Symptom.** Windows CI: `remove vectors db: Os { code: 32, kind: Uncategorized,
message: "The process cannot access the file because it is being used by another
process." }` right after `wait_for_ann` reported the ANN as published. Linux and
macOS never fail.

**Root cause (a real bug, not a test flake).** `build_in_background` published
the `Ann` entry and only *then* re-opened `VectorCache` to release the backfill
lease. So there was a window where callers already saw the ANN while the build
thread still held an open `vectors.db` connection. On Windows an open handle is
a sharing violation for `remove_file`; on Unix `unlink` of an open file just
works. Any purge (`models remove`, cache clear) racing the tail of a build hit
the same thing in production.

**Fix.** The build closure now returns the loaded index and the publish happens
*after* the lease release, so "ANN visible" implies "no `vectors.db` handle
held". The existing test's `remove_file` is the regression check (Windows only,
since Unix cannot observe it) and carries a comment naming the ordering
invariant so it is not reintroduced.

**Ruling.** Not a timeout to raise and not a retry to add: fix the ordering once
in the shared code path.

## Flake #9 — `search_runs_during_the_build_and_ann_serves_the_read_path`

**Symptom.** `hybrid_rank_ann`: `search during the build must stream exact, not
consult the ANN` failed while the same test passes on an idle machine. The test
took 9.0 s when it failed and 11–13 s when it passed locally, but **29.5 s on
the `linux-hybrid` runner**.

**Root cause.** The test held the background ANN build open with a fixed
`set_test_build_delay(1_500)` and then measured "did the search consult the
ANN". On a slow host the *search itself* outlasts 1.5 s, the build finishes
first, and the assertion fires. This is the same host-relative-timing mistake
the latency gate was fixed for (FR-22 Delta 2026-10-29), in test form.

**Fix.** The delay hook is replaced by a gate (`TestBuildGate` /
`set_test_build_gate`): the build blocks at its start until the test opens the
gate, which happens only after the during-build assertions. The guarantee is now
structural — no constant to outrun — and the hook waits with a 120 s timeout so
a panicking test cannot wedge the build thread.

**Ruling.** Never pin "still in flight" with a sleep; use a gate. A sleep can
only be right for one host speed.

## Flake #10 — `watcher_recovers_from_a_commit_failure` (Windows)

**Symptom.** Windows `ci` on the merge of #55:
`only the rebuild marker may interrupt the healing window: project error: index
…\projects\1\tantivy is missing (hint: run docsbase index to rebuild)`.

**Root cause (real, not a test artifact).** The heal path
(`create_fresh`) removes the whole index directory and then rewrites
`docsbase.rebuild` inside it. Between the removal and the rewrite there was no
marker *and* no `meta.json`, so `ReadIndex::open` fell through to its "index is
missing" branch and told the user to run `docsbase index` — while the daemon was
already rebuilding. On Linux that window is microseconds; on Windows the
directory removal itself retries for up to 2 s, so a 50 ms poll loop lands in it.

**Fix.** The rebuild state is also recorded in a sibling marker
(`<project>/docsbase.rebuild`), written before the directory is removed and
cleared by `mark_rebuilt`. The project directory is never deleted by the heal, so
a reader sees "needs to be rebuilt" for the entire window. `ReadIndex::open` and
`open_or_create` consult either marker.

**Ruling.** A state that must be visible while a directory is being destroyed
cannot live inside that directory.

## Flake #11 — `hybrid_rank_ann::search_runs_during_the_build_and_ann_serves_the_read_path` (macOS)

**Symptom.** macOS `ci` on #57 (a docs-only change):
`a published ANN must serve the read path`, with
`warning: ann returned 37 candidates for fetch 50; falling back to exact`.

**Root cause 1 — the fake embedder was degenerate.** `LengthEmbedder` put the
text length in dim 0 and a constant in dim 1, so every point of the 12k-chunk
corpus sat on one line, thousands of them near-duplicates. `hnsw_rs` builds a
nondeterministic graph over that, and its search sometimes reached only 37 of
the 50 requested neighbours. The read path then fell back to exact — the
documented behaviour — and the assertion failed for a reason unrelated to
publishing. The vectors are now FNV-1a seeded and L2-normalized, so the graph is
well connected and the assertion tests the read path.

**Root cause 2 — the "oversized fetch" sub-assertion was unsatisfiable by
design.** `fetch = limit.max(window).min(MAX_HITS)` with `MAX_HITS = 1000`
(`daemon/tools.rs:258`), so asking a 12k-chunk project for `chunks + 1` becomes a
request for 1000 — which the graph answers. The assertion therefore passed only
when the build had not published yet, i.e. it flaked on timing. The check now
lives in its own test with a 20-document corpus (~40 chunks), where the clamp
genuinely exceeds the graph.

**Ruling.** A fake embedder must produce spread vectors — collinear duplicates
turn an ANN assertion into a lottery on graph construction. And an assertion
about "the ANN cannot satisfy this fetch" has to be expressed *within* the
clamp, never above it: above the clamp the request is rewritten and the premise
disappears.

## Flake #12 — `install::concurrent_stops_do_not_wedge` (linux-local)

**Symptom.** `linux-local`, combined-feature suite:
`concurrent stop must succeed` at `tests/install.rs:491` — one of four
concurrent `docsbase daemon stop` runs exited non-zero. The test discarded the
command's stderr, so the reason was invisible.

**Candidates, and what was done.** The failure was not reproduced locally
(48 + 120 + 360 concurrent-stop invocations, 0 failures), so this is **not a
proven root cause** — it is two races removed plus a diagnostic added:

1. `cleanup_stale` did `state.exists()` then `fs::remove_file()`: a TOCTOU pair.
   When several stops observe a dead daemon, the first removes the state file
   and the rest got `ENOENT`, which surfaced as a client error. Removal is now
   `NotFound`-tolerant (`platform::remove` for the endpoint already was).
2. `stop_daemon` propagated `write_all`/`flush` failures. A write can fail
   because the peer is already shutting down and closed the connection — a stop
   under way, not a client error. The loop now ignores the write result and lets
   the process-exit poll decide, which is what "did it stop" means.

**Diagnostic.** The test now carries each stop's stderr into the assertion
message, so the next occurrence names the failing path instead of just "must
succeed".

**Ruling.** Cleanup that any number of processes may run concurrently must treat
`NotFound` as success — and a test that asserts on a subprocess must print that
subprocess's stderr, otherwise a rare failure is unactionable.
