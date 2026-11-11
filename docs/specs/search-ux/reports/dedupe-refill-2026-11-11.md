# #68 — результаты bounded refill, 2026-11-11

База061b353 (#118), реализация286b9df. Rust1.88 / cli-full / release с LTO;
Linux x86_64 sandbox. Новые зависимости, baseline/threshold/model/recording
изменения отсутствуют; paid/live запросов нет.

## Контракт и корректность

Real SQLite/Tantivy regression40 сильных chunks одного файла +10 более слабых
одночастных файлов: до изменения limit10/cap2 давал2 hits; теперь10. Все
50 chunks доходят до единственного rerank, включая сороковой, который test
provider ставит первым. Lexical/RRF/rerank, scoped и unfiltered dense пути
покрыты настоящим search_docs. RRF scores/order сверены с полными source lists.

Source exhaustion отличается от candidate budget. Для единственной lexical
страницы нет filler. При1010 доминирующих chunks бюджет1000 даёт2 hits и
explicit candidate-budget. Настройка40 сохраняет короткий base40; limit50
увеличивает обычный base до200 даже при заданном refill ceiling40.

Clamped RRF10→12: четыре chunks приобретают второй source contribution,
обычный top12 выбрасывает два base candidates. Новый selector сохраняет
все10 base chunks, оставляя только2 места для дополнения и общий score order.

## Release-диагностика

Команды (owner cache/toolchain paths задаются окружением):

```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features cli-full --release --test dedupe_refill release_refill_diagnostics -- --ignored --nocapture
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features cli-full --release --test scoped_retrieval_perf -- --ignored --nocapture
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features cli-full --release --test search_bench dedupe_slice_diagnostics -- --nocapture
```

Синтетические2D vectors, scoped guide/; query/provider caches создаются на
новый сценарий, затем повторный warm запрос. First не означает physical disk/OS
cold. Это отдельные observations, не p95, production-corpus или remote latency.
В корне есть ещё один нерелевантный файл вне scope, в таблице — eligible docs.

| Сценарий | Eligible docs / chunks | Lexical first / warm, ms | RRF first / warm, ms | Hits | Window / rounds / final pool |
|---|---|---|---|---|---|
| Без добора |11 /11 |0.939 /0.653 |2.616 /2.341 |10 |40 /1 /11 |
|40 dominant +10 |11 /50 |1.353 /1.015 |3.914 /3.120 |10 |160 /2 /50 |
|1010 dominant +10 |11 /1020 |7.176 /7.175 |12.637 /11.841 |2 |1000 /4 /1000 |

Dense source запрашивается до1000 один раз; фактических vectors может быть
меньше. Lexical widths40→160→640→1000, сумма1840; retained/provider payload
<=1000 chunks. Long-chunk collector по-прежнему может исследовать больше raw
matches: это ширина candidates, не hard postings/CPU budget.

## Выбор алгоритма и память

Lexical group collector способен избежать повторных поисковых проходов и
держать только лучшие cap chunks. Это самостоятельный перспективный путь,
но до rerank он теряет возможный лучший chunk, а `doc_id` нынешнего Tantivy
индекса stored+indexed, без FAST: эффективная per-match группировка потребует
обоснования доступа к doc identity (либо format/rebuild change). В этом PR
общий bounded path выбран ради одинакового контракта всех stages, защиты
base pool и отсутствия schema migration. Здесь нет заявления, что он быстрее
не реализованного эквивалентного group collector. Более глубокий exhaustive
lexical grouping и адаптивное планирование остаются design work #117.

Числовые source lists <=1000 каждая, временная fusion union<=2000, final pool
<=effective ceiling; base-ID set ограничен прежним base window. Full text
копируется один раз после preflight/RAII byte admission, scope admission
освобождается до payload. Это cardinality/logical-byte evidence, не peak query
RSS claim. Process resource checks проверяются отдельно.

Все новые lexical phases снова допускаются с прежней generation/config и
original deadline; stale generation/cancel/byte refusal не публикуют partial
ответ и не вызывают rerank. Cleanup tests используют равные request/aggregate
лимиты и затем резервируют всю aggregate capacity. Никаких shared planner
caches или новых invalidation callbacks нет.


## 20k файлов, стоимость cap без lexical refill

Настоящий indexed corpus:20k файлов, по10k в a/ и b/, по одному chunk;
все synthetic vectors2D. Порядок каждой пары fixed plain RRF→cap RRF, так
что cap имеет преимущество прогретых OS/SQL/vector-cache страниц. First
обозначает первый запрос сцены, не физически холодный cap lookup.

| Query / scope | Plain RRF first / warm, ms | Cap RRF first / warm, ms | Final pool / rounds |
|---|---|---|---|
|shared / b/ |196.880 /192.456 |191.256 /200.113 |40 /1 |
|rareword / b/ |193.359 /205.094 |192.184 /192.919 |40 /1 |
|shared / b/09999.md |24.379 /17.976 |17.558 /16.652 |1 /1 |

На этих отдельных observations увеличение запрошенного dense heap40→1000
не добавило отдельного full-vector scan и не показало крупной задержки сверх
существующего streaming~190–205ms. Эти данные не доказывают универсальную
эквивалентность производительности, ANN/высокие dims/paid rerank не измерены.
RRF cap всегда запрашивает dense1000 по умолчанию, даже если final pool40 уже
достаточен; экономия CPU/ANN beam через адаптивный planner остаётся #117.

Legacy algorithm-only query-first reference всё ещё печатается для контекста
#67; это не эквивалентный end-to-end scope/admission/provider pipeline, и он
не используется как baseline/claim об ускорении #68.


## ru_morph (12 queries)

Release diagnostic повторён без перезаписи baseline.

| Cap | duplicate_path_ratio | Expected-path hits |
|---|---|---|
|none |0.582 |11 |
|1 |0.000 |8 |
|2 (default) |0.357 |10 |
|3 |0.480 |12 |
|4 |0.526 |12 |

Кривая совпала с прежним diagnostic. Это не исправление morphology/representation
issues и не повод менять default2 или fixtures. Report-only mean latency на этом
срезе: plain0.56ms, snippet+cap0.55ms; отдельный baseline gate проверяется обычной
full suite, не заменяется этим малым измерением.


## Verification

MSRV1.88 cli-full full suite: **1200 passed,15 ignored**,71 target groups;
`metrics_match_baseline` прошёл на неизменённом baseline. Clippy all-targets
и explicit artifact/perf targets, fmt passed. Свежая static CLI: **20535168B**,
static linkage и обе CLI provider capabilities passed. **6 release resource
checks** passed, включая idle RSS,50k-chunk search,10k-document sync/RSS.
Lean dependency tree не содержит network/ML crates; offline syscall test
и6 Python CI-observer tests passed. New runtime settings/index formats не
подменяют эти gates и не повышают пороги.

Final fixture cleanup закрывает SQLite до удаления TempDirs на Windows.
Повторный focused run и Clippy фиксируются в task ledger. Required native CI
и финальный merge проверяются после публикации PR на его точном HEAD.

Durable sources: новые real-search regressions `tests/dedupe_refill.rs`,
`tests/search_payload_budget.rs`; RRF adversarial unit в
`src/daemon/search_refill.rs`; manual20k probe в
`tests/scoped_retrieval_perf.rs`. Local logs: /tmp/refill-release-probe.log,
/tmp/refill-20k-probe.log, /tmp/refill-ru-morph.log,
/tmp/refill-delivery-checks.log, /tmp/refill-final-focused.log.
