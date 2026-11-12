# CF4 — границы offline planning после CF3

Дата: 2026-11-11. Issue #78. Исследование следующего implementation slice;
не новая гарантия runtime. Исходный код: 62d4ced (CF3), delivery — PR #127.
New crates: none. Запросы к providers, загрузка моделей и изменение baseline
не входят в этот этап.

## Вопросы, которые нельзя скрыть за общим ChangePlan

| Вопрос | Что подтверждено кодом | Следствие для реализации |
|---|---|---|
| Что означает Ready у нынешнего inspector? | `store/inspect.rs::schema` читает header100bytes и metadata, не SQL rows | Подтверждена только schema identity; нельзя объявлять пригодными chunks/registry/lexical/vector snapshot |
| Можно ли взять обычный readonly Db для dry-run? | Operational readers допускают SQLite sidecars; pure inspector намеренно не открывает Connection | Для missing/schema mismatch/WAL остаются только bounded no-write observations; Unknown не маскируется |
| Откуда получить полный registry survey? | Pure header inspector не перечисляет проекты; CF3 принимает явный caller-supplied список | CLI не может назвать cwd или несколько переданных roots полной registry inventory; unavailable observation блокирует полную migration |
| Достаточно ли передать пустой global TOML при отсутствующем файле? | CF3 SourceLayer содержит supplied text, SourceEdit hash не описывает file existence | На filesystem boundary различать observed-missing и observed-empty; будущий apply проверяет соответствующий precondition |
| Совпадают ли current files и live daemon config? | Shared удерживает Arc<Config> прежнего open-time snapshot; происхождение не сохраняется целиком | Source effective view и active-runtime observation должны иметь разные basis; origins нельзя реконструировать по поздним файлам |
| Можно ли взять новый panel DTO? | PR126 `ConfigView` явно отдаёт source=None; panel_projects дополнительно вызывает project_config/coverage | Это полезный UI contract, но не pure config/artifact inspector и не доказательство provenance |
| Уже ли есть generic artifact DAG? | `ComponentImpact` пока только Unchanged/InitializeRuntime/ReconfigureRuntime; ArtifactId — component/project | Целевой design более богатый, его нельзя принять за существующую реализацию |
| Инвалидирует ли timeout vectors? | Матрица state-dependencies отделяет runtime от artifact edges | Timeout/resource-policy change не создаёт vector fetch/ANN rebuild; общий hash Config не заменяет owner identities |
| Является ли source hash semantic fingerprint? | CF3 source hash относится к exact before bytes, включая comments | Stale-source revision отделяется от semantic identities; credentials и их hashes не входят в semantic fingerprints |
| Что показывать для ещё не реализованного apply? | CF5/CF7/CF11 отсутствуют | План показывает blocked/unavailable adoption, не успешно выполненное действие; plan_id не является разрешением |

## Предлагаемая последовательность CF4

1. Зафиксировать формат effective view и basis наблюдения, составляющие identity,
   invariants action graph и no-write observation boundary. Нужен отдельный
   implementation plan с конкретными типами/owning modules и RED cases.
2. Реализовать pure planner/effective-view kernel вместе с реальными owner
   assessments и inspector acceptance. Не ограничиваться тестом generic mock DAG.
   Дедупликация должна сохранять различие eviction/delete/rebuild и причины;
   cycles/conflicts/missing dependencies отклоняются до apply. Unsupported actions
   явно blocked; unknown identity не становится Ready/reuse.
3. Подключить versioned read-only CLI отдельно от прежнего `docsbase config`.
   Registry observation и missing-global preconditions должны иметь правдивый
   контракт до появления полного source migration output. Не вводить hidden
   autostart, read-write recovery, provider constructor или небounded DB copy.
4. Проверить реальные filesystem snapshots (включая directories/WAL/SHM/conflict
   logs), invalid/missing schema, no-provider counters, релевантные/нерелевантные
   изменения, canonical plan IDs, feature-off и CLI compatibility. Fresh review,
   ledger и native CI обязательны для каждого законченного implementation task.

## Решения, которые остаются частью следующего design task

- Безопасный источник registry observations для CLI при существующей БД, особенно
  при WAL. Не выбирать immutable=1 или full DB copy только ради завершения API.
- Минимальная структура ArtifactId/action identities для реальных current owners
  и будущих model/generation slices без обещания несуществующего active snapshot.
- Owner-provided effective values: descriptor/units/origins берутся у owner;
  coordinator не копирует правила ANN/provider/settings. Показывать только
  реально разрешённые данные, отдельно указать ещё не внедрённые owners.
- Бюджет planner input/output: bounded metadata, без vector payload; лимиты
  принадлежат owner и разрешаются до planner, не скрываются в runtime fallback.

Реализация CF4 не начата. Этот документ сохраняет обнаруженные границы и вопросы,
а не помечает FR-6/8/9/10/11/12 или issue #78 выполненными.
