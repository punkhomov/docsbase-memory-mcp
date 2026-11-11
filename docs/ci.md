# CI: профили сборки и проверки

Дата: 2026-11-09. Текущая структура — C11; исторические deltas ниже сохраняют
контекст предыдущих этапов. Обновлять вместе с workflows и ci-testing specs.

## Поставляемый CLI и расписание

Cargo bundle `cli-full` включает `hybrid-local` и `hybrid-remote`. Официальные
release builds и полные native suites на Linux/macOS/Windows используют его.
`default = []` — поддерживаемая lean/source конфигурация. Модели и ONNX Runtime
устанавливаются явно; обычный поиск без hybrid-конфига остается offline.

| Workflow / job | Когда | Проверки |
|---|---|---|
| `ci / docs` | PR, push main, manual | файлы, installers/version, observer regressions |
| `ci / linux` | PR, push main, manual | полный cli-full: fmt/Clippy/tests, search-quality gate, lean dependency boundary, static artifact и оба provider smokes |
| `ci / macos` | PR, push main, manual | полный cli-full и реальные filesystem/process semantics macOS |
| `ci / windows` | PR, push main, manual | полный cli-full и реальные filesystem/IPC/handle semantics Windows |
| `feature-compatibility / feature-compatibility` | понедельник 08:17 Europe/Samara (04:17 UTC), manual | remote/local/combined lints, depth2 powerset, feature-off selections, полный lean suite; одна Linux job |
| `nightly / release-checks` | ежедневно 07:17 Europe/Samara (03:17 UTC), при новых commits; manual всегда | release cli-full, изолированный perf, offline/platform/static gates |
| `release` | version tag или manual | сборка/упаковка CLI; публикация только для tag |

Required checks — ровно `docs`, `linux`, `macos`, `windows`; их имена и настройки
защиты main сохраняются. Source compatibility workflow не запускается на PR или
push main и не блокирует merge. Его ошибки остаются видимыми как обычный красный
run, без `continue-on-error`. Source configurations сохраняют поддержку, но
feature-off регрессия может обнаружиться после merge, вплоть до недели.
Дешевый dependency-tree gate продолжает защищать lean dependency boundary на PR.

Новая возможность поставляемого CLI включается в bundle; все три native jobs
проверяют ее на соответствующей ОС. При правке cfg/features можно вручную
проверить secondary source configurations до merge:

```sh
gh workflow run feature-compatibility.yml --ref <branch>
```

GitHub должен сначала зарегистрировать новый workflow на default branch;
до первого merge dispatch из ветки может быть недоступен. Команды этого
первоначального изменения проверяются локально, ограничение фиксируется в PR.

## Проверка фактического бинарника

`.github/scripts/check-cli-capabilities.py <binary>` запускает реальный CLI
с пустыми config/data/cache и без OPENROUTER_API_KEY/ORT_DYLIB_PATH. Проверяет
local model command и remote credential-validation path. Не требует сети,
моделей и установленной ORT. Native CI проверяет свой test-build binary;
release workflow распаковывает созданный archive и проверяет файл из него.
Linux release-static alias включает cli-full и сохраняет static linking and size measurements.
Windows/macOS release build указывает cli-full явно.

Так проверка не зависит от того, с какими features собран сам тестовый harness.
Checksum-verified опубликованный alpha.5 воспроизводит исходную ошибку: local
команды отсутствуют, remote сообщает missing build feature.

## Feature и platform оси

Один full build не проверяет выключенные cfg branches. Lean/individual feature
compatibility проверяется одной weekly/manual Linux job. Cargo-hack powerset
перебирает независимые hybrid-local/hybrid-remote; umbrella cli-full исключён
из enumeration, поскольку его native jobs проверяют напрямую.

Платформенный provisioning и lifecycle должны исполняться на соответствующей ОС:
compile-only не проверяет rename-over-existing, блокировки, mmap cleanup,
сигналы и права. Модельные runtime tests герметичны: fakes, без downloads/ORT dylib.
Live model test остаётся manual/ignored по принятому решению.

## Кеши

Swatinem/rust-cache остаётся отдельным для каждой job/ОС/компилятора. Кеш —
ускорение подготовки, а не источник доказательства состава CLI. Разные feature
configurations имеют разные compiled artifacts. Передача archive test binaries
между runners требует отдельного измерения и не является частью этого фикса.

## Тайминги: бюджеты host-relative

CI-раннеры — общие VM и стабильно в 2–2.5 раза медленнее машины разработчика
(измерено 2026-10-29: один и тот же гейт-тест `search_bench` — 6.5 s локально
и 12.5–13 s на `ubuntu-latest`; Rrf p95 13.16 ms в baseline против
26.7–33.0 ms в CI). Отсюда два правила:

- latency-гейт применяет `baseline_p95 × 1.5 × DOCSBASE_BENCH_LATENCY_SLACK + 5 ms`
  (Linux CI задаёт 3; без переменной, в том числе в текущих macOS/Windows jobs,
  используется 1.0; абсолютный потолок 500 ms не масштабируется) —
  `docs/specs/search-benchmark/requirements.md`, FR-22, Delta 2026-10-29;
- **нельзя ужесточать бюджет по локальному прогону** и нельзя делать вывод
  «код стал медленнее» из красного CI-тайминга.

Таймауты в тестах — тоже бюджеты. Тест, который ждёт асинхронное событие
(вотчер, демон, ANN-билд, IPC), обязан иметь запас на медленный раннер: сейчас
это `watcher_deadline()` в `tests/config_runtime.rs` (60 s на Windows, 20 s на
Unix), бюджет ожидания ANN в `tests/ann_e2e.rs` (300 s на Windows, 60 s
иначе — фоновый билд в debug-бинаре на общем раннере не укладывался в 60 s)
и `wait_until(30s)`-хелперы. Сам timeout не различает нехватку ресурсов,
зависание и регрессию: сообщение обязано печатать, сколько ждали и в каком
состоянии застали систему. Причину устанавливают по диагностике, а не
повышением лимита до зелёного результата. Известные флейки и их root cause —
`docs/daemon-test-flakes.md`; стресс-прогон — `flake-stress.yml` (только
вручную, никогда не часть PR).

## Изменения проверок

Сохранять required checks docs/linux/macos/windows и обязательность search gate.
Новая возможность поставляемого CLI добавляется в cli-full; smoke/behavior tests
должны защищать фактические handlers. При изменении jobs обновлять эту таблицу.
Baseline и timing policy не регенерировать как побочный эффект изменений CI.

## Delta 2026-11-08 — наблюдаемость перед перестройкой CI (C1)

Первый этап сохраняет шесть существующих PR jobs, triggers, features, test
selectors, ignored tests и бюджеты. `docs/linux/macos/windows` остаются required.
`linux-hybrid` и `linux-local` пока продолжают запускаться на PR/push main;
перенос compatibility в weekly/manual — следующий отдельный delta, вместе с
обновлением обещаний поддерживаемых source-build configurations.

Каждый прежний функциональный `cargo test` теперь имеет три явных шага:

1. **Prepare:** тот же набор + `--no-run --timings`. Cargo компилирует и линкует
   тестовые бинарники, сохраняет HTML compile timings.
2. **Inventory:** тот же набор + `-- --list`. Сохраняет найденные имена тестов
   и их test target; тела тестов не выполняются. Перечень отражает cfg этой ОС
   и features. Он включает ignored-тесты, но не доказывает их исполнение.
3. **Execute:** исходная команда без изменения параметров. Обычный Cargo runner,
   его exit code определяет результат шага. Rustdoc остаётся частью Cargo flow.

Все шаги используют тот же target directory. Повторный вызов Cargo проверяет
актуальность и переиспользует совместимые результаты. `--no-run` не компилирует
doctests; inventory/execute могут выполнять работу rustdoc. Clippy выполняется
раньше и уже прогревает часть графа: prepare — не время полной сборки с нуля.

### Карта текущего покрытия

Это логические группы, **не allowlist для выбора тестов**. Полные suites
по-прежнему подхватывают новые Cargo test targets автоматически. Точный состав
для конкретной ОС/конфигурации хранится в inventory artifact этого запуска.

| Область | Источники проверок | Текущее исполнение |
|---|---|---|
| Библиотечные контракты | unit tests в `src/**`, doctests | native `cli-full` на трёх ОС; также lean suite |
| Parsing, store, индекс, базовый поиск | `chunker`, `frontmatter`, `tokenizer`, `walk`, `store`, `tantivy_index`, `search_golden`, `smoke` | native full suites; также lean |
| ANN, векторный кеш, ранжирование | `ann_*`, `hybrid_ann`, `hybrid_rank_ann`, `hybrid_ranking`, `vector_cache` | native full suites с действующими cfg/ignore |
| Индексация, watcher, VCS | `index_*`, `cli_index`, `watcher`, `watch_retry`, `git_worktrees`, `vcs` | native full suites на реальной ОС |
| Daemon, admission, IPC, MCP, lifecycle | `admission*`, `cleanup`, `cli_routing`, `ipc_*`, `lifecycle`, `mcp_*`, `platform_*`, `registry`, `status` | native full suites с платформенными cfg |
| Конфигурация, CLI, providers | `config*`, `cli_models`, `cli_read`, `connect`, `error_*`, `hybrid_config/jobs/local/models/openrouter`, `install` | native full; feature-sensitive selections в compatibility lanes |
| Качество поиска | `search_bench::metrics_match_baseline` и остальные offline benchmark assertions | остаётся обязательным внутри `linux`; выполняется также там, где его включает full suite |
| Артефакт и offline-инварианты | `artifact`, `offline` | `artifact` — opt-in, явный static build/check в required Linux и nightly/release; `offline` сохраняет Linux cfg |
| Performance и внешние/длительные проверки | `perf_budget`, `soak`, `hybrid_live`, `hybrid_local_live`, `search_bench_live` | `perf_budget` — opt-in target, release nightly; остальные сохраняют ignore/cfg, soak/live — weekly/manual, local live — manual |

В C1 selected compatibility suites находились в `ci.yml`; C11 переносит те же
команды в `feature-compatibility.yml`. Nightly/release и performance routing
при этом остаются прежними.

### Как читать результаты

`.github/scripts/observe-tests.py` использует Python stdlib. В
`target/ci-observations/<suite>/` он сохраняет для каждой фазы:

- `<phase>.log`: объединённые stdout/stderr с немедленным flush;
- `<phase>.json`: команда, revision/run/attempt/job, ОС/CPU, reported cache hit,
  разрешённые публичные build settings, wall time, exit code, inventory и
  распознанные libtest target summaries. Полное окружение не записывается.

Observer выводит UTF-8 и при перенаправлении stdout на Windows, чтобы сообщения
с не-латинским текстом не прерывали Cargo из-за системной кодировки Python.

Во время тишины каждые 30 s появляется heartbeat с elapsed time и последней
строкой вывода; одновременно обновляется JSON. До завершения `status=running`,
`exit_code=null`. Это наблюдение, а не дополнительный timeout или SLA.
Интервал задаётся аргументом `--heartbeat-seconds` observer script.

GitHub job summary показывает время каждой фазы и самые долгие test targets.
Artifact `test-diagnostics-<job>-<attempt>` содержит logs/JSON и
`target/cargo-timings`, retention 14 дней. Upload вызывается с `always()`;
жёсткое завершение runner/job или сбой upload всё равно могут помешать доставке.
При обычном падении исходный ненулевой exit code остаётся ошибкой CI.

Ограничения измерения:

- libtest сообщает время **тестового бинарника**, не отдельного теста. Оно
  включает его fixtures/ожидания и параллельное исполнение; суммировать эти
  времена как независимые per-test measurements нельзя;
- execute wall time включает остаточную работу Cargo, rustdoc и nested builds.
  До C10 `artifact` запускал `cargo release-static` внутри execute. Теперь
  static build измеряется отдельно — см. Delta C10 ниже;
- cache hit — результат cache action, не доказательство отсутствия компиляции.
  Сравнивать нужно actual compile timings, ревизии, features, runner и состояние
  кеша. По этим данным нельзя автоматически объявить прогон cold или warm;
- inventory и длительности не доказывают правильность assertions. Аудит
  контрактов, управляемое время и воспроизводимый порядок потоков — отдельная
  работа; высокий test count не заменяет эти проверки.

Для локального воспроизведения, например:

```sh
python3 .github/scripts/observe-tests.py --suite cli-full --phase prepare -- cargo test --locked --features cli-full --no-run --timings
python3 .github/scripts/observe-tests.py --suite cli-full --phase inventory -- cargo test --locked --features cli-full -- --list
python3 .github/scripts/observe-tests.py --suite cli-full --phase execute -- cargo test --locked --features cli-full
```

Python regression checks: `python3 -m unittest discover -s .github/scripts -p 'test_*.py'`.
Спека и ledger: `docs/specs/ci-testing/`.

## Delta 2026-11-09 — малые ANN-фикстуры и performance target (C2/C3)

Проверки пустого запроса и несовпадающей размерности ANN используют два
ненулевых 2D-вектора и положительный контроль валидного запроса. Mismatch
проверяется в обе стороны. Большие 5,000×64 recall/roundtrip сценарии и пороги
качества сохранены; пересечение ID в high-fetch recall вычисляется через set.
Принятие неверного запроса как успешного пустого результата проверено временной
mutation: оба теста падают; production-код восстановлен без изменений.

`perf_budget` объявлен в Cargo.toml с `test = false`: target запускается явно.
Nightly выполняет две последовательные группы:

```sh
cargo test --locked --features cli-full --release --test perf_budget -- --nocapture --test-threads=1
cargo test --locked --features cli-full --release
```

Первый вызов исполняет все шесть ресурсных проверок ровно один раз, без
конкурирующих аллокаций соседних тестов того же процесса. Второй запускает все
остальные default targets и doctests; Cargo автоматически подхватывает новые
targets. Раньше второй вызов повторял perf с обычным параллелизмом.

`cargo test --release` сам по себе теперь запускает функциональный release
набор. Для полного локального контроля нужно выполнить обе команды выше.
Обычный debug suite также не включает шесть ранее ignored perf-тестов; их
тела, ignore attributes и budgets не изменены.

Cargo `--all-targets` тоже исключает opt-in target (даже вместе с `--test`).
Поэтому Linux PR job и nightly дополнительно выполняют отдельный вызов:

```sh
cargo clippy --locked --features cli-full --test perf_budget -- -D warnings
```

Так Linux-only performance-код продолжает компилироваться и проверяться до
merge. Новый job не добавляется; подготовленные результаты переиспользуются.
Остальные PR suites, features, required checks, search gate, schedules и
таймауты сохранены. Дубликат perf устраняется без изменения численных бюджетов.

## Delta 2026-11-09 — завершение daemon refresh и граница ANN (C5)

Приватный refresh возвращает handle своего worker; обычные callers сразу
отпускают его и сохраняют фоновое выполнение. Завершение этого worker означает
окончание warm/cleanup orchestration; сам ANN build остаётся отдельной задачей.
Тесты наблюдают warm через локальный callback на том же config/DB/cleanup пути.
Production callback — прежний `DenseCache::warm`; глобальных test hooks нет.

Положительный контроль каналами удерживает warm и доказывает, что refresh уже
вернул handle, а cleanup ещё не начался. Для lexical (default и explicit) и
disabled hybrid тест дожидается worker и проверяет ноль warm-вызовов плюс
реальное удаление stale tmp. Фиксированного ожидания30s для отсутствия READY
больше нет. Ошибка warm и реальная ошибка project cleanup не должны мешать
disk cleanup; это проверяется с маленьким per-fixture disk budget.

Отдельная интеграция сохраняет default admission: один Markdown-файл сначала
даёт `ANN_MIN_VECTORS - 1` чанков, затем ровно `ANN_MIN_VECTORS`. Реальные counts
проверяются в БД. Ниже порога остаётся exact, на границе refresh публикует ANN.
Ожидание публикации читает READY/`ann_snapshot`, не вызывает `get`, который мог
бы сам инициировать build. Вместо почти коллинеарных fake embeddings используются
детерминированные нормированные векторы.

Оставшиеся deadlines — watchdogs реального worker/build, заканчивающиеся на
событии. Runtime defaults, config resolution, большие recall/scale suites,
численные бюджеты и структура jobs не меняются. Точечные контролируемые
мутации проверяют чувствительность новых assertions; это локальная проверка,
а не дополнительный обязательный mutation job в каждом PR.

## Delta 2026-11-09 — компактный ANN search-path corpus (C6)

`hybrid_rank_ann` сохраняет 600 разных документов и 64-мерные нормированные
векторы, но каждый короткий документ теперь дает один уникальный чанк. Counts
и уникальность hashes проверяются по БД. Раньше растущие тела из повторяемого
текста давали 12 660 чанков; для проверки пути поиска такой размер не требовался.
Малый проект содержит 20 документов/чанков и не может удовлетворить fetch,
равный `MAX_HITS`. Корни проектов живут до завершения поисков.

Существующие build gate, RAII release и ANN-served counter сохранены. Во время
сборки результат сравнивается с отдельным ANN-disabled exact-контролем. После
публикации ANN действительно обслуживает повторяемый поиск с десятью
результатами. Недостаточный граф должен сообщить `ann-insufficient`, вернуть
exact fallback и не увеличить ANN counter. Приближенный ANN-результат не обязан
совпадать с exact; большие recall/scale suites сохраняют прежние пороги качества.

Численные defaults, per-fixture admission, fetch/limit, watchdog, единственный
debug test target и маршрутизация CI сохранены. Фазовые локальные замеры и
контролируемые mutations служат проверкой изменения, а не новыми CI budgets.

## Delta 2026-11-09 — запись конфига без sleep-порядка (C7)

`concurrent_global_writers_both_survive` больше не предполагает, что соседний
поток успел дойти до нужной фазы за50ms. A удерживает настоящий fd-lock до
подтверждения, что B уже пытался взять его и получил contention. Затем A
публикует embedding, освобождает lock, и только после этого B продолжает retry.
Проверяется полный TOML: оба этапа, их параметры, ranking и посторонние секции.
Обычный project write также проходит через реальный production entry.

В старом тесте A записывал конфиг перед паузой400ms: перенесенное перед lock
чтение B уже могло увидеть embedding, и неверный алгоритм оставался зеленым.
Эта слепая зона воспроизведена controlled mutation. Новый тест удерживает файл
неизмененным до подтвержденного contention и обнаруживает такую потерю данных.

Приватная граница model write и внутренний config lock принимают локальный
`FnMut(Duration)` ожидания. Production передает `std::thread::sleep`; число
попыток, задержка, реальные OS locks, data-dir routing и guard lifetime прежние.
В тесте callback подтверждает неудачную попытку и ждет разрешения через канал.
Shared env guard защищает lookup data-dir; новых глобальных hooks/переменных нет.

Проверки retry-политики удерживают настоящий lock и записывают запрошенные
задержки вместо реального backoff. Проверяются исчерпание бюджета без записи,
успех без ожидания после release и успех следующей попытки при release из
wait callback. Оставшиеся таймауты — реальные watchdogs каналов/worker.
Численные defaults, CLI semantics и состав jobs не меняются. Watcher и HTTP
Retry-After/backoff остаются следующими отдельными частями аудита.

## Delta 2026-11-09 — наблюдаемый отказ stale-schema watcher (C8)

`stale_schema_survives_file_edit` сначала подтверждает успешную обработку
здоровой правки: committed chunks/generation и результат клиентского поиска
изменились. Затем сохраняет snapshot данных, понижает schema stamp и меняет
README. Второй новый Markdown-путь, отсутствовавший ранее, служит свидетельством
новой работы после downgrade. Тест дожидается terminal error jobs нужного
проекта с id после cursor, rebuild-hint и каноническими путями обеих правок.
Пути могут попасть в один или разные debounce batches; порядок raw OS events
и число callback-вызовов не фиксируются.

Это использует существующие persisted failed-batch diagnostics. Старый тест
оставался зеленым даже при молчаливом пропуске stale batch: пауза2.5s не
доказывала работу consumer. Теперь отсутствие отказов или неверные пути не
дают пройти тесту. До/после подтверждения проверяются stale stamp и неизменные
идентичности/хеши документов, тексты чанков и generation. Свежий read-only reader
дополнительно проверяет сохранение здорового контента и отсутствие stale edits
в физическом индексе; публичный поиск по-прежнему требует полный rebuild.

Production watcher/notify/debounce/admission не меняются; новых API/hooks/
зависимостей/конфигурационных настроек нет. Условные ожидания ограничены реальным
watchdog и завершаются на наблюдаемом прогрессе, без обязательного quiet period.
Усиление чувствительности отдельно от обещаний ускорить весь CI.


## Delta 2026-11-09 — детерминированная HTTP retry-политика (C9)

Старый `retry_after_is_honored_on_429` задавал нулевой header и проверял только
результат/число запросов. Он проходил даже при игнорировании Retry-After.
Теперь unit-тесты записывают точный порядок POST, jitter и запрошенных задержек:
ненулевой header, ноль, отсутствие,30s cap до jitter, рост backoff после header,
смешанные HTTP/transport failures и завершение без лишнего ожидания. Четыре
попытки и последняя ошибка, включая ее source, проверяются отдельно.

Приватный per-call helper получает callbacks ожидания/jitter; обычный публичный
вызов передает прежние `thread::sleep` и генератор jitter. Численные defaults,
публичный API и production-политика прежние. Policy-тесты не тратят backoff и
не сравнивают wall-clock время. Существующий4s backoff cap при четырех попытках
недостижим; дополнительный режим ради его тестирования не вводится.

Два медленных клиентских теста исчерпания заменены более строгими unit-тестами,
которые входят в обязательный cli-full на всех трех ОС. Клиентский503→успех
сохраняет обычное реальное ожидание и wiring,429 smoke проверяет retry/result
без заявления о доказанной задержке. Recorded fixtures, parsing/request/error
проверки и blocking guard сохранены. Состав jobs, features и бюджеты прежние.


## Delta 2026-11-09 — явная сборка static artifact (C10)

`artifact` теперь opt-in target (`test = false`). Обычный функциональный
`cargo test` не собирает static release CLI из теста. Required Linux сохраняет
обе проверки — static linking и измерение размера — после отдельного успешного
`cargo release-static --locked`. Этот alias по-прежнему включает cli-full.
Оба providers дополнительно проверяются offline именно в static binary;
прежний smoke debug CLI и проверка packaged release CLI остаются.

Build имеет собственный отчет `static-artifact-build/prepare.json`; checker
использует отдельные `static-artifact/{prepare,inventory,execute}.json` и logs.
Все попадают в прежний always-upload diagnostics artifact. Поэтому время
release-компиляции больше не скрывается во времени функциональных тестов.
Это разделение фаз и устранение дублирования, не обещание ускорения всех jobs.

Checker читает только `target/x86_64-unknown-linux-gnu/release/docsbase`
(или соответствующий CARGO_TARGET_DIR), ничего не собирает и не подменяет
файл host binary. Отсутствующий файл дает инструкцию build-first. Наличие файла
не доказывает свежесть: workflows всегда сначала успешно запускают Cargo,
а локально нужен тот же порядок:

```sh
cargo release-static --locked
cargo test --locked --features cli-full --test artifact -- --nocapture
python3 .github/scripts/check-cli-capabilities.py target/x86_64-unknown-linux-gnu/release/docsbase
```

Static checker требует ELF magic и непустой вывод ldd с допустимым кодом0/1,
затем исключает динамические зависимости. Код1 нужен для корректного GNU
static executable. Само сообщение «not a dynamic executable» недостаточно:
ldd пишет его и для текстового файла. Пустой и oversized файл отклоняет size
check; размерный бюджет прежний. Реальный build раньше уже давал ELF — это
защита новой границы потребления готового файла, не дефект production-сборки.

Nightly явно строит и проверяет static CLI; функциональный release suite и
изолированный perf сохраняются. Release workflow переиспользует свой build,
checker больше не запускает Cargo повторно. В required Linux и nightly есть
явный Clippy для artifact: `--all-targets` не включает opt-in tests. Остальные
native/lean functional tests и doctests обнаруживаются обычным Cargo. Только
artifact уходит из default inventories, включая два повторных исполнения в
lean compatibility. На macOS/Windows этот Linux target прежде был пустым.
Jobs/triggers, profiles, release packaging/attestations, defaults, dependencies,
остальные проверки и бюджеты не меняются.


## Delta 2026-11-09 — четыре PR jobs и периодическая compatibility (C11)

Из ci.yml удалены только linux-hybrid/linux-local. Все шаги четырех native/docs
jobs сохранены; в Linux добавлен дешевый default dependency gate. Он сначала
сохраняет plain `cargo tree --locked --color never -e normal`, затем проверяет
непустой граф и прежний blacklist. Исходный pipeline внутри if мог считать
ошибку Cargo успешным отсутствием forbidden crates. Теперь producer failure
не маскируется, а граф попадает в прежний diagnostics artifact.

Те же13Cargo/observer шагов перенесены в одну job нового weekly/manual workflow:
remote-only lint/selected tests; local-only lint/selected tests; прежний powerset
check depth2 с исключенным umbrella cli-full; полный lean suite; combined lint.
Один checkout/toolchain/cache и target-dir обслуживают последовательные configs.
Сохраняются suites hybrid-remote/hybrid-local/lean, prepare/inventory/execute,
slack3, always-upload и обычное завершение с ошибкой.60минут — суммарный job cap
двух прежних30минутных jobs; тестовые таймауты и бюджеты не меняются. Отдельного
gate job нет: workflow запускается каждую неделю даже без новых commits.

Это согласованное изменение срока обнаружения source-only регрессий. Оно явно
заменяет старый T7 ruling «linux-local должен стать required». Штатный CLI
по-прежнему проходит полный required native CI на трех ОС; тяжелые ANN,
watcher/lifecycle, search-quality и artifact assertions сохраняются. Новых
feature×OS матриц, моделей/API credentials, зависимостей и production changes
нет. Live/local-model tests остаются manual/ignored; nightly/release/perf
расписания и команды не меняются. Не делаем вывода об ускорении всего CI из
одного прогона: меняется структура и устраняется лишняя PR работа.


C11 local verification также выявила отдельную слабость get_doc frame-теста:
перед проверкой он индексировал2MB через короткий2s IPC deadline, а текстовый
assert принимал и daemon error, и Transport-отказ клиента на слишком большой
frame. Теперь tiny index готовится CLI до старта демона, большой файл пишется
до watcher startup, и проверяется bounded daemon Protocol error плюс следующий
успешный запрос тем же соединением. Escaped data гарантированно превышает wire
cap; исходный raw size допустим fixture config. Имя/coverage сохранены,
production и deadlines прежние. Old/new assertions проверены на временно
ослабленном guard; production восстановлен. Это уточнение теста, не новый
дефект daemon framing. Детали: FR-40 и ledger.


## Delta 2026-11-11 — manual ANN recall investigation (#109)

`ANN recall investigation` (`ann-investigation.yml`) — отдельный manual-only
macOS workflow для воспроизведения нестабильного качества HNSW. Он выполняет
cli-full ann_recall suite до указанного числа раз (default20, integer1..50)
и завершает job с failure при первом nonzero exit. Это stress observation,
не rerun-until-green. Логи всех выполненных trials и failing graph bundle
попадают в artifact с retention14d. Required ci.yml, её tests/scheduling и
пороги качества сохраняются. Успешная серия не закрывает #109 без root cause.

Обычный separated-cluster test при quality failure сохраняет граф и queries
в уже загружаемый `target/ci-observations/ann-recall`. Replay-команда и
ограничения: `docs/specs/ann-index/reports/recall-investigation-2026-11-11.md`.


## Delta 2026-11-11 — Web acceptance

Native jobs explicitly prepare the same locked Svelte bundle before cli-full.
Linux runs Playwright against the already built debug binary, with no retries;
traces/screenshots/console/Web logs are retained. Each native capability smoke
also opens the embedded Web artifact from an empty directory. Default dependency
boundary additionally excludes axum/ts-rs. The 40 MiB cap is removed; Cargo/Vite
report bundle bytes, artifact tests report executable bytes, release packaging
reports archive bytes. Historical cap references are superseded.
