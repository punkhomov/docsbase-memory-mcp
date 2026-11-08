# Повторная adjudication: #70, 2026-11-08

Пересмотрены `hn-19`, `hard-07`, `hard-10`; отдельно проверены контрольные
`ar-01`, `hard-08`, `hn-22`. Исправляется разметка релевантности, поэтому
изменение её метрик не означает улучшение поискового движка.

## Исходные данные и метод

Исходный commit: `40132c80fd335017101ddd5c5c7f2cfe48736800`, Rust 1.88.0,
TOKENIZER_VERSION=7. [Пул](adjudication-2026-11-08-pool.json) сохраняет
исходные corpus/queries hashes и оба model pins. В нём 99 уникальных
path/line фрагментов: 23/18/15 для спорных запросов и 14/14/15 для контролей.
Позиции взяты из объединения top-10 Bm25, Bm25Rerank, Rrf, RrfRerank.
Исторический queries hash относится к разметке **до** этой коррекции.

Исходный отчёт получен без provider inference:

```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test search_bench metrics_match_baseline -- --exact --nocapture
```

Артефакт: `target/search-bench/gate-report.json`. При воспроизведении
исходного пула нужен указанный commit. На общем sandbox-хосте использован
документированный `DOCSBASE_BENCH_LATENCY_SLACK=3`; бюджет не менялся.

Проверялись полные документы правдоподобных альтернатив, а не только
совпавшая строка. Наличие заголовка в пуле позволяло найти релевантный body,
но сам заголовок не получал оценку за непоказанный фрагмент. Неразмеченные
кандидаты не объявлялись отрицательными автоматически.

Рубрикатор FR-2/FR-26: grade 2 — самостоятельный прямой ответ; grade 1 —
частичный или условный ответ, ограничение которого указано явно.

Reviewer provenance:

- `issue70-author` — автор этого изменения, проверивший пул и контекст.
- `issue70-adjudicator` — свежий независимый reviewer-агент
  `/root/issue70_adjudicator`; прочитал исходные qrels, полный пул и документы
  до изменения фикстур. Его рекомендации приняты с обоснованиями ниже.

Оба имени добавляются к прежним reviewer tags шести проверенных кейсов.

## hn-19: staging host smoke checks after restore

Все пути в таблицах относятся к `tests/fixtures/search-bench/corpus/`.

| Expected path | Lines | Grade | Основание |
|---|---|---:|---|
| `multi/backup-full.md` | 10–12 | 2 | Развернуть копию на стенде и выполнить дымовые проверки. |
| `semantic/backup-restore.md` | 6–6 | 1 | Проверка restore на staging host; конкретный smoke-набор не указан. |
| `boundary/index-rebuild.md` | 68–70 | 1 | Условная процедура smoke queries после restore-triggered rebuild. |

Прежний whole-document absent на `index-rebuild.md` удалён. Его причина
описывала только marker self-healing и пропускала фактически поднятый
фрагмент 66–71. Полный контекст также существенен: строки 85–87 говорят,
что восстановление бэкапа с прерванной перестройкой завершает rebuild;
строки 68–70 предписывают smoke queries после **каждой** перестройки.
Это условно полезная инструкция проверки после такого restore.

Граница решения: staging host и общий drill проверки бэкапа в этом
документе не заданы. Поэтому это grade 1, а не замена прямого ответа
`backup-full`. Исходная гипотеза автора о нерелевантности smoke checks
после rebuild пересмотрена после независимого анализа раздела 85–87.

`backup-restore` раньше стоял первым в трёх режимах, но не учитывался как
релевантный. Тело 83–87 `index-rebuild` само по себе описывает восстановление,
а не процедуру проверки: его не подменяют новым expected span 68–70.
Checksums **до** restore, замер RTO, описание процедуры восстановления,
runtime health probes и supervisor cadence не отвечают на smoke validation
после restore. Они остаются неразмеченными near-matches.

## hard-07: default port

| Expected path | Lines | Grade | Основание |
|---|---|---:|---|
| `traps/default-port.md` | 5–6 | 2 | Числовое значение default: 8374. |
| `traps/port-env.md` | 5–8 | 1 | Override precedence и возврат к default; числового значения нет. |

Прежний whole-document absent на `port-env.md` удалён. Запрос не говорит
«fixed production port» или «исключить overrides», поэтому такую границу
нельзя дописать в intent задним числом. Упоминание compiled-in default и
возврата к нему через unset — поддерживающая информация.

Синтетические документы противоречат друг другу относительно
configurability в production. Этот пересмотр не согласует их факты и не
выдаёт их за единое описание настоящей конфигурации: broad query допускает
обе темы, но только первый документ сообщает число. Retention defaults,
install defaults, frame sizes, tool defaults и socket/pipe paths на вопрос
о default port не отвечают.

## hard-10: timeout in seconds

| Expected path | Lines | Grade | Основание |
|---|---|---:|---|
| `traps/timeout-sec.md` | 5–6 | 2 | CLI display в целых секундах. |
| `traps/timeout-ms.md` | 5–8 | 1 | Default thirty seconds при внутреннем учёте в ms. |
| `boundary/daemon-protocol.md` | 54–56 | 2 | Конкретный per-call deadline: 30 seconds. |
| `en/frozen-mcp.md` | 41–43 | 2 | Конкретные IPC/polling timeout values в секундах. |
| `multi/timeout-connect.md` | 10–11 | 2 | Рекомендуемые connection timeouts: три/десять секунд. |
| `boundary/daemon-protocol.md` | 71–72 | 1 | Контекстный ten-second shutdown grace deadline. |
| `en/child-processes.md` | 20–21 | 1 | Контекстный ten-second drain deadline перед kill. |

Прежний whole-document absent на `timeout-ms.md` удалён. Query не уточняет
CLI display против internal accounting. В обоих исходных документах
упомянуты обе единицы; фраза «thirty seconds in milliseconds» поддерживает
вопрос о значении в секундах, хотя не объясняет display/configuration в них.

Ruling: broad query включает seconds display и конкретные timeout durations,
выраженные в секундах. Shutdown/drain deadlines оставлены grade 1: это
ограниченные контекстом grace periods, а не общее правило единиц измерения.
Probe interval из `child-processes:35–36` не является timeout и не добавлен.
Ms-only query/read/socket passages и чтение timeout settings из config не
дают seconds-specific ответа и не добавлены в expected.

`multi/timeout-connect.md` вошёл в пул только заголовком 1–2. Его прямой
ответ 10–11 намеренно включён в qrels: заголовок остаётся unmatched,
поэтому эта полезная альтернатива выявляет реальный citation miss и может
уменьшить recall/nDCG после расширения expected. Выбор qrels не ограничен
уже успешно показанными фрагментами.

## Контрольные контрасты сохранены

| Case | Expected | Absent | Независимая проверка |
|---|---|---|---|
| `ar-01` | `ar/library.md:5–5`, grade 2 | Whole `ar/office.md` | مكتبة означает библиотеку; офисные часы и meeting room отвечают о другом объекте. |
| `hard-08` | `traps/port-env.md:5–6`, grade 2 | Whole `traps/default-port.md` | Query явно называет `PORT`; документ о fixed default не описывает эту переменную и её precedence. |
| `hn-22` | `traps/config-toml.md:5–6`, grade 2 | Whole `traps/config-json.md` | Query явно называет TOML; JSON игнорирует unknown keys, TOML отклоняет их. |

В этих запросах различающий объект/идентификатор/формат указан явно.
Прежние expected/absent, диапазоны, grades и reasons сохраняются. Их
известные ошибки остаются отдельными дефектами retrieval/ranking, в том
числе Arabic same-root trap из #47. Остальные pooled candidates контролей
не дают новых релевантных альтернатив.

## Состав срезов и принятие baseline

Три пересмотренных ID сохраняются, но класс меняется на `multi_relevant`:
обоснованной opposite-behavior ловушки при буквальных queries больше нет.
Размеры срезов: `hard_negative` 22 → 19, `multi_relevant` 18 → 21; всего
по-прежнему 193 запроса. Query texts и k сохраняются во всех кейсах.

Оценки меняются по трём причинам: добавлены поддерживающие/прямые ответы,
устранены необоснованные absent, изменился состав двух срезов. Это требует
явной обновлённой baseline, а не scoring changes. В частности, пять known
allowances для двух rerank-режимов hard-07/hard-10 и RrfRerank hn-19
теряют основание **из-за коррекции разметки**, а не исправления движка.

При принятии baseline проверяются все 772 ranked lists, неизменность
контрольных метрик и остальных 187 кейсов, corpus/recording hashes,
discrimination и оставшиеся четыре control allowances. Latency и index
build reference сохраняются от прежней baseline: этот пересмотр не
перекалибровывает performance. Recording-time aggregate metadata не
переписывается; точные query/chunk texts и ordered replay pools прежние.

Выводы относятся к этим трём спорным кейсам и трём контролям. Независимая
оценка общего качества и её внешняя проверка остаются направлением #72.

## Измеренная коррекция baseline
Повторный offline replay подтвердил неизменность всех **772 ranked lists**.
187 непересмотренных case objects побайтно идентичны; у трёх контролей
qrels и все quality-метрики идентичны. Во всех 193 cases сохранены query
и k. Corpus, legacy, recordings, model pins и frozen latency сохранены.

| Режим | Hit@10 до → после (из 193) | Mean nDCG@10 до → после |
|---|---|---|
| Bm25 | 134 → 135 | 0.593554 → 0.592580 |
| Bm25Rerank | 140 → 141 | 0.667069 → 0.669060 |
| Rrf | 165 → 165 | 0.637431 → 0.637591 |
| RrfRerank | 183 → 183 | 0.853529 → 0.855253 |

Полный per-case diff трёх изменённых оценок:

| Case | Режим | Recall@10 до → после | nDCG@10 до → после |
|---|---|---|---|
| hard-07 | Bm25 | 1.000000 → 1.000000 | 1.000000 → 1.000000 |
| hard-10 | Bm25 | 1.000000 → 0.571429 | 1.000000 → 0.570000 |
| hn-19 | Bm25 | 0.000000 → 0.333333 | 0.000000 → 0.242076 |
| hard-07 | Bm25Rerank | 1.000000 → 1.000000 | 0.500000 → 0.688529 |
| hard-10 | Bm25Rerank | 1.000000 → 0.571429 | 0.630930 → 0.584619 |
| hn-19 | Bm25Rerank | 0.000000 → 0.333333 | 0.000000 → 0.242076 |
| hard-07 | Rrf | 1.000000 → 1.000000 | 0.500000 → 0.586883 |
| hard-10 | Rrf | 1.000000 → 0.428571 | 0.630930 → 0.376190 |
| hn-19 | Rrf | 1.000000 → 1.000000 | 0.500000 → 0.698839 |
| hard-07 | RrfRerank | 1.000000 → 1.000000 | 0.500000 → 0.688529 |
| hard-10 | RrfRerank | 1.000000 → 0.571429 | 0.630930 → 0.584619 |
| hn-19 | RrfRerank | 1.000000 → 1.000000 | 0.630930 → 0.821314 |

Изменения MRR/overlap следуют тем же новым path/span judgments; точные
значения находятся в reviewed diff `baseline.json`. В частности, снижение
recall/nDCG у hard-10 ожидаемо при добавлении ранее пропущенного evidence
в знаменатель: оно сохранено в baseline и не скрыто за общим средним.

Known allowances: **9 → 4**, новых нет. Retired: Bm25Rerank/RrfRerank
hard-07 и hard-10, RrfRerank hn-19. Сохранены Bm25 ar-01, Rrf hard-08,
Bm25Rerank/RrfRerank hn-22. Ни одна из этих четырёх ошибок не объявлена
исправленной вследствие review неоднозначных queries.

Saturated cases: **133/193 → 134/193** (68.91% → 69.43%), ниже порога 70%.
Все остальные discrimination conditions проверены штатным gate.

Перед regeneration штатный provenance gate упал с `HashMismatch` на новом
raw queries hash; затем явный regeneration без accept-new-known сообщил
`+0 new, -5 fixed`. Старые latency/index-build reference values восстановлены
после regeneration как осознанный performance-neutral шаг. Весь
`search_bench` после принятия baseline: 78 passed, 2 intentionally ignored.
