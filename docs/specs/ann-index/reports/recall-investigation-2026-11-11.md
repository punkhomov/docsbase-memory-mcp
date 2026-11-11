# #109: расследование recall — 2026-11-11

Исходники production: main `2cfec2a8e11914f5fe20be8878b86d67dc52add6`.
Среда: Linux x86_64, Rust1.88.0, hnsw_rs0.3.5/anndists0.1.5.

## Вывод

Исходный macOS сбой membership0.945 подтверждён логом run42815248295,
job129826571322, macos-26-arm64. Локально он **не воспроизведён**. Ни гонка
первого entry point, ни потеря связности, ни search beam пока не установлены
причиной. Изменение runtime HNSW-параметров на этих данных не оправдано.

## Выполненные probes

- 100 отдельных исполнений исходной public ann_recall cluster проверки:
 по20 при RAYON_NUM_THREADS=2/4/8/16/32,0 failures.
- 15 полных ann_recall suites: по5 при Rayon3/4/8, test threads4,
 150 test executions,0 failures.
- 80 direct pinned-HNSW debug builds: по40 при Rayon8/16,
 5000×64, те же M16/layers16/ef_construction200; minimum membership1.000.
- 400 direct optimized pinned-HNSW builds: по200 при Rayon3/8,
 тот же корпус5000×64; minimum membership1.000.
- 300 serial/permuted builds меньшего корпуса1000×64:100 debug,
 200 optimized; minimum membership1.000. Это дополнительные варианты входа,
 а не точное воспроизведение исходного fixture.

Direct probes используют те же deterministic LCG vectors и public dependency
API, но не заменяют полный production path. Все workloads синтетические;
latency/RSS выводов, cross-platform гарантии или production p95 здесь нет.
Новых provider calls не было. Смена количества потоков меняет scheduling,
а не обеспечивает детерминированность parallel construction.

Аудит pinned source: initial entry-point read/publication выполняются в
разных critical sections; уровни генерируются seeded RNG, но назначаются
точкам по scheduling; reverse-neighbour shrinking и upper-layer search
также могут влиять на путь обхода. Эти наблюдения остаются гипотезами.

## Новый диагностический контракт

При штатном quality failure test сохраняет observations.json с actual query
vectors и IDs/scores, platform/available_parallelism/Rayon override/source SHA,
а также уже построенный ANN через существующий dump API. Bundle попадает
в existing required CI artifact target/ci-observations. Диагностическая
ошибка лишь печатается: исходный test всё равно падает. Successful test не
создаёт bundle.

После скачивания artifact передать точный каталог failure-*:

```sh
DOCSBASE_ANN_REPLAY_BUNDLE=/absolute/path/to/ann-recall/failure-... \
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features cli-full --test ann_recall \
  -- --ignored --exact replay_captured_separated_cluster_failure --nocapture
```

Replay загружает original graph, а не строит новый. Он проверяет membership
и число hits; прежние IDs/scores сохраняются для сравнения, но не навязывают
будущей исправленной реализации прежний неправильный порядок. Если причина
в construction, исправление должно отдельно рассмотреть старые persisted
graphs и invalidation/rebuild. Новый diagnostic code on-disk production
identity не меняет.

Manual workflow `ANN recall investigation`: только macOS, default20 полных
suites, accepted1..50, первый failure прерывает серию с красным статусом.
Retention artifact14 дней. Штатные required jobs и пороги не меняются.

## Проверка milestone

Локально Rust1.88 cli-full: 1201 passed,16 ignored в71 test groups;
unchanged metrics_match_baseline прошёл. fmt и clippy all-targets чистые.
Capture/drop/reload/search control прошёл; explicit replay дополнительно
проверен на двух настоящих20-point graphs: positive membership1.000 зелёный,
negative control с queries из противоположного кластера membership0.000
падает с exit101. Это проверка диагностического механизма, не воспроизведение
исходной ANN-ошибки. Embedded Python manual workflow проверен синтаксически.
Fresh review и native CI добавляются в ledger/PR.
#109 остаётся открытой до установленной причины и проверенного исправления.
