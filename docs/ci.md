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
  (в CI фактор 3, локально 1.0; абсолютный потолок 500 ms не масштабируется) —
  `docs/specs/search-benchmark/requirements.md`, FR-22, Delta 2026-10-29;
- **нельзя ужесточать бюджет по локальному прогону** и нельзя делать вывод
  «код стал медленнее» из красного CI-тайминга.

Таймауты в тестах — тоже бюджеты. Тест, который ждёт асинхронное событие
(вотчер, демон, ANN-билд, IPC), обязан иметь запас на медленный раннер: сейчас
это `watcher_deadline()` в `tests/config_runtime.rs` (60 s на Windows, 20 s на
Unix), бюджет ожидания ANN в `tests/ann_e2e.rs` (300 s на Windows, 60 s
иначе — фоновый билд в debug-бинаре на общем раннере не укладывался в 60 s)
и `wait_until(30s)`-хелперы. Провал таких тестов почти всегда означает
нехватку бюджета, а не регрессию: сообщение обязано печатать, сколько ждали и
в каком состоянии застали систему. Известные флейки и их root cause —
`docs/daemon-test-flakes.md`; стресс-прогон — `flake-stress.yml` (только
вручную, никогда не часть PR).

## Изменения проверок

Сохранять required checks docs/linux/macos/windows и обязательность search gate.
Новая возможность поставляемого CLI добавляется в cli-full; smoke/behavior tests
должны защищать фактические handlers. При изменении jobs обновлять эту таблицу.
Baseline и timing policy не регенерировать как побочный эффект изменений CI.
