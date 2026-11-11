# План M5 — 2026-11-11: безопасная публикация test launcher

Исполнение: `superpowers:executing-plans`, один последовательный task.

**Цель:** устранить перезапись уже запускавшегося executable из fixture
`tests/install.rs::Env::install` без изменения production installer.

**Основание:** Apple, [Updating Mac software](https://developer.apple.com/documentation/security/updating-mac-software):
signature cache привязан к старому файлу; безопасный updater пишет новый
файл и заменяет старый rename. В нашей fixture повторный install вызывает
`fs::copy` поверх существующего launcher. Native probe run43049798365:
50 overwrite /50 fresh-name /50 unlink executions,0 failures; overwrite
сохраняет inode49 раз. Это воспроизведение опасной границы, не исходного
SIGKILL. #113 остаётся открытой: termination cause не установлена.

**Ограничения:** Rust1.88, существующий tempfile, test-only diff; никаких
новых dependencies, production/config/daemon changes, codesign repair,
SIGKILL retries, ослабления assertions/CI или baseline changes.

## M5 — staging + atomic replacement в fixture

Файлы: `tests/install.rs`, dated deltas requirements/design/tasks/progress,
отчёт `installer-copy-investigation-2026-11-11.md`.

- [x] RED: выделить copy в test-only helper и сохранить hard-link alias
  прежнего target; после публикации alias должен читать прежние bytes,
  а path — новые. На прежнем fs::copy alias видит новые bytes. Начальный
  held-open вариант не portable: Windows PermissionDenied5, run43051934156.
- [x] GREEN: NamedTempFile в том же parent → into_temp_path (writer закрыт)
  → fs::copy → TempPath::persist поверх target. При ошибке staging guard
  удаляет temporary file, существующий target не переписывается.
- [ ] Прежняя install idempotence/ownership/atomic-update coverage остаётся.
  Проверить focused install, fmt/clippy, затем required native checks и
  fresh task/final reviews. Scope не включает переписывание ci.yml.
- [ ] Отдельно записать границу выводов и native stress evidence; не закрывать
  #113 по устранению одной подтверждённой опасной операции.
