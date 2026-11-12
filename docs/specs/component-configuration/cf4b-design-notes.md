# CF4b — проектирование pure change plan

Дата: 2026-11-12. Продолжение #78 после CF4a; Rig/#128 не является зависимостью.
Исходные design notes; конкретный реализуемый контракт закреплён в
[cf4b-plan.md](cf4b-plan.md) и dated deltas requirements/design. Не новый runtime.

## Нужный результат

Pure planner принимает explicit observations и owner assessments, выдаёт
versioned ChangePlan с deterministic identity, упорядоченными действиями,
причинами, prerequisites, grants и blockers. Никаких файлов/provider/runtime
операций внутри graph compiler. Реальные owner adapters обязательны: generic
DAG, проверенный только mock узлами, не закрывает этап.

Первые реальные потребители:

1. Изменение уже существующих SearchSettings/DbSettings: owner impact даёт
   initialize/reconfigure runtime policy. Изменение timeout не создаёт действий
   над vectors/ANN/lexical. Возможность выполнить update пока не заявляется.
2. CF3 MigrationDraft: source rewrite зависит от принятого shared-store adoption;
   пока CF7 отсутствует, оно явно blocked. Неполный survey остаётся blocker,
   пустой список edits у incomplete draft не означает «изменений не требуется».
3. Текущий no-write metadata-schema inspector: Ready относится только к header
   identity. Missing/Unknown/NeedsRecovery различаются; наличие header не
   доказывает registry inventory, активное поколение или пригодность поиска.

## Invariants

- Whole-config hash не используется как vector/ANN identity. Source revisions
  (exact-byte stale check) отделены от semantic fingerprints компонент.
- Plan ID — versioned/domain-separated canonical digest, не authorization token.
  Сортировка входных assessments/facts не должна менять ID; изменение relevant
  input/reason/grant/blocker должно менять canonical plan content.
- Повтор идентичного действия дедуплицируется; причины и dependencies объединяются
  детерминированно. Конфликтующие определения одного действия, недостающие
  dependencies и cycles отклоняются до публикации плана.
- Evict/DeleteStore/Rebuild — разные эффекты. Их нельзя склеивать по имени cache.
  Сочетание несовместимых действий над одним resource требует явного owner
  решения, не произвольного порядка coordinator.
- Unknown/missing identity не превращается в Ready/reuse. Неподдерживаемый
  fingerprint format не сравнивается как известная compatible identity.
- Неподдерживаемые executor/actions видны как blocked. Требуемые grants и факт
  реализации executor различаются; наличие plan_id/selected budget не даёт consent.
- Planner limits передаются явно до compilation, без fallback внутри runtime:
  action/edge count и serialized metadata bytes. Plan не читает vector payload.
- Ошибка не меняет caller inputs, sources, database/sidecars/conflict logs.

## Решения перед кодом

Нужно закрепить минимальные структуры action/resource identity и owner assessment
на реальных SearchSpec/DbSpec/MigrationDraft use cases, затем записать interfaces,
точные file changes и RED acceptance в cf4b-plan.md. Не вводить заранее универсальную
plugin framework или центральную копию правил provider/ANN.

CLI integration и полный registry survey остаются отдельной границей: current
header inspector не умеет читать registered roots. Независимый read-only plan
не может объявить supplied cwd/roots полным registry и не должен открывать
operational Db ради удобства. Missing global file и observed empty source также
получают разные будущие stale preconditions. Конкретный безопасный механизм
registry observation должен быть выбран до полного migration CLI.
