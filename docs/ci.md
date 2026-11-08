# CI: профили сборки и проверки

Дата: 2026-11-08. Delta: shipped CLI использует `cli-full`. Обновлять вместе
с `.github/workflows/ci.yml` и `.github/workflows/release.yml`.

## Поставляемый CLI

Cargo bundle `cli-full` включает `hybrid-local` и `hybrid-remote`. Его используют
официальные release builds и native PR suites на Linux/macOS/Windows.
`default = []` — lean/source конфигурация; её dependency gate сохраняется,
но релизные бинарники должны предоставлять оба provider. Модели и ONNX Runtime
устанавливаются явно, обычный поиск без hybrid-конфига остаётся offline.

| Job | Проверки | Гарантия |
|---|---|---|
| `docs` | файлы, installers, version | гигиена репозитория |
| `linux` | fmt, clippy/test `cli-full`, capabilities smoke | полный CLI и обязательный search-quality gate |
| `macos` | fmt, clippy/test `cli-full`, capabilities smoke | тот же CLI и реальная macOS filesystem/process семантика |
| `windows` | fmt, clippy/test `cli-full`, capabilities smoke | тот же CLI и реальная Windows filesystem/IPC/handle семантика |
| `linux-hybrid` | remote-only lint/selected tests, lean dependency tree | feature-off assertions и отсутствие ML/network crates в lean Cargo build |
| `linux-local` | local-only lint/selected tests, powerset check, lean full suite, combo lint | совместимость исходных feature configurations |
| `nightly` | release tests `cli-full`, perf/offline/platform gates | release-профиль и ресурсные бюджеты |

Compatibility lanes сохранены в этом исправлении поставки. Их дальнейшая
организация и оптимизация ожиданий/фикстур — отдельная работа. Полная combo
runtime suite теперь выполняется native jobs через cli-full; повтор в
linux-local заменён lean suite, чтобы сохранить feature-off runtime coverage.
Отдельные local-lib/default прогоны на Windows/macOS заменены одним полным
cli-full прогоном, который включает provisioning-тесты.

## Проверка фактического бинарника

`.github/scripts/check-cli-capabilities.py <binary>` запускает реальный CLI
с пустыми config/data/cache и без OPENROUTER_API_KEY/ORT_DYLIB_PATH. Проверяет
local model command и remote credential-validation path. Не требует сети,
моделей и установленной ORT. Native CI проверяет свой test-build binary;
release workflow распаковывает созданный archive и проверяет файл из него.
Linux release-static alias включает cli-full и сохраняет static/40 MiB checks.
Windows/macOS release build указывает cli-full явно.

Так проверка не зависит от того, с какими features собран сам тестовый harness.
Checksum-verified опубликованный alpha.5 воспроизводит исходную ошибку: local
команды отсутствуют, remote сообщает missing build feature.

## Feature и platform оси

Один full build не проверяет выключенные cfg branches. Lean/individual feature
compatibility проверяется существующими Linux lanes. Cargo-hack powerset
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
| Артефакт и offline-инварианты | `artifact`, `offline` | действующие Linux cfg; artifact пока строит release binary из теста |
| Performance и внешние/длительные проверки | `perf_budget`, `soak`, `hybrid_live`, `hybrid_local_live`, `search_bench_live` | PR сохраняет обычную ignore/cfg политику; release budgets — nightly, soak/live — отдельные weekly/manual workflows; local live — manual |

Точные selected suites в compatibility lanes остаются в `ci.yml`. C1 не
переносит дорогие проверки в nightly и не меняет исполнение этих workflows.

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
  В частности, `artifact` всё ещё запускает `cargo release-static`; отдельная
  явная сборка артефакта будет следующим изменением;
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
