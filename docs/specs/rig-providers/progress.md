# Progress

2026-11-12: RIG1 in progress. Отдельный worktree feat/rig-providers от main
543225b, Web PR #126 не включён. ADR-RIG1 и границы согласованы с намерениями #128.

2026-11-12: OpenRouter runtime переведён на Rig Wire encode/decode; single-shot
probes остаются прежними. 26 remote fixture/contract tests PASS. Новый тест
на duplicate rerank indices сначала упал на старом пути (RED), затем PASS.
Все 12 новых lock entries проверены по sparse pubtime: >=14 дней; resolver
nightly 1.102 также отклонил свежий Rig 0.46.

MSRV 1.97.1: fmt и cli-full all-targets clippy + opt-in artifact/perf_budget
clippy PASS. Механические lint fixes включены без изменения длительностей.
Lean/local dependency trees не содержат rig-core, rig-http, reqwest.
Полный functional suite, отдельные feature checks и native CI ещё выполняются.

Среда: служебная /tmp/.git схлопывала независимые fixture roots в один project.
Контрольный тест воспроизводит ошибку в /tmp и проходит с TMPDIR=/var/tmp.
Первый полный прогон остановлен; повторный использует /var/tmp. Это ограничение
среды, код registry/search не менялся для его обхода.

2026-11-12: validation fix — Rig включает serde_json/preserve_order глобально.
Полный прогон дошёл до search_golden и выявил только порядок ключей JSON:
все 4 .snap.new равны исходным snapshots после JSON parsing. Canonicalization
сортирует ключи объектов, но сохраняет порядок массивов, ranking и baseline.
Повторный golden: 4/4 PASS. Полный повторный suite и native CI продолжаются.
Первый независимый review: качество APPROVED, нет implementation findings;
приёмка ожидает завершения полного suite/native CI.

2026-11-12: local validation complete на 7d41ae6. Полный cli-full suite:
1240 PASS, 0 FAIL, 18 штатно ignored; TMPDIR=/var/tmp и документированный
DOCSBASE_BENCH_LATENCY_SLACK=3. Golden: 4/4 PASS в cli-full и default, ни один
snapshot/baseline не изменён. Первый latency прогон с factor=1 совпал с release
компиляцией и превысил относительный бюджет; thresholds не менялись.

Default/hybrid-local all-targets check и clippy, cli-full all-targets clippy,
opt-in artifact/perf_budget clippy, fmt, diff-check, actionlint и 6 CI observer
tests PASS. Release-static и оба artifact checks PASS: 20 398 880 bytes,
статическая линковка. Debug/static CLI capability checks PASS. Повторная
проверка всех 12 новых lock records по sparse index pubtime подтвердила cooldown.
Manual/live/soak tests и отдельный release perf_budget runtime не запускались.

Независимый re-review 41339d6..7d41ae6: golden finding ADDRESSED; новых
Critical/Important нет. Первый native CI 43219312076 выявил тот же порядок
ключей JSON на Linux/macOS; исправление проверяет CI 43221257106.

2026-11-12: Task RIG1: complete (FR-1–FR-7, SC-1–SC-3).
Native CI [43221257106](https://github.com/punkhomov/docsbase-memory-mcp/actions/runs/43221257106)
на 7d41ae6: docs/linux/macos/windows SUCCESS, включая Linux static artifact.
Финальный независимый review whole-branch diff: нет Critical/Important/Minor;
единственный открытый пункт приёмки был native CI и теперь подтверждён.
Ruling: первый этап реализован и проверен; deferred minors: none.

PR [#130](https://github.com/punkhomov/docsbase-memory-mcp/pull/130) содержит
готовый этап. После записи этого ledger required checks на последнем коммите
остаются обязательным условием ready-for-review; их текущий статус хранит PR.
Merge — только человеком. #128 и #81 остаются открыты для следующих этапов;
Web PR #126 не включён и не изменён.

2026-11-12: после приёмки main обновился до f70a6f8 (#131, effective config view).
Rebase прошёл без конфликтов: range-diff подтвердил идентичность всех трёх
патчей. На новой базе cli-full all-targets clippy и config_view (6) + golden (4)
PASS. Реализация Rig не изменялась; проверка обновлённой базы выполняется
required CI на текущем head PR #130. Предыдущие SHA выше — исторические
идентификаторы реально проверенных коммитов до rebase, а не текущий head.
