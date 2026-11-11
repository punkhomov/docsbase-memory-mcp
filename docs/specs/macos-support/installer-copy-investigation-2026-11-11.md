# #113: launcher-copy investigation — 2026-11-11

Исходный main:0d75c3d. macOS symptom остаётся SIGKILL9 с пустыми stdout/stderr
в Env::install. Исходные данные не различают первый и idempotent повторный
install и не содержат termination namespace. Причина пока не установлена.

## Что проверено

В tests/install.rs install_owned_artifacts вызывает Env::install дважды.
Каждый вызов выполнял fs::copy в тот же launcher. Apple прямо предупреждает
об изменении signed executable in place и рекомендует новый файл + rename:
https://developer.apple.com/documentation/security/updating-mac-software.
Внешние аналогичные отчёты (не доказательство нашего случая):
https://github.com/dotnet/install-scripts/pull/743 и
https://github.com/doordash-oss/doordash-cli/issues/159.

Native manual experiment run43049798365 на scratch HEAD84780ea:
https://github.com/punkhomov/docsbase-memory-mcp/actions/runs/43049798365

- 50 fs::copy overwrites:1 inode,49 in-place rewrites,50 successes.
- 50 fresh names:50 distinct inodes,50 successes.
- 50 unlink/copy:50 distinct inodes,50 successes.
- Source executable78821240 bytes; observed command--version, не install.

Это150 synthetic launcher executions, не150 install tests. Исходный SIGKILL
не повторён. Probe ограничивает disk одним launcher; первый run43049556633
отменён ещё при compile, до execute, чтобы устранить накопление50 больших
copies. Его результаты не включены в150. Scratch workflow override не входит
в main/этот PR; main ANN workflow и required ci.yml сохраняются.

## Изменение M5

Fixture публикует launcher через существующий tempfile и same-directory
atomic replacement. Реальный regression удерживает hard-link alias старого
файла: исходный fs::copy изменяет наблюдаемые через alias bytes (RED),
staging/rename сохраняет их и публикует новые по target path (GREEN). Исправляется именно unsafe
copy boundary в test utility, production installer не меняется.

Локально Rust1.88 cli-full install:16 passed (все15 прежних +1 regression).
All-targets Clippy чистый; fmt и native CI/review фиксируются в PR close-out.
Нет изменений dependencies, baseline, runtime/config или дополнительных
subprocess retries. No migration/recovery action. #113 остаётся открытой:
нужны actual failed PID/phase и macOS termination evidence либо новое
воспроизведение, чтобы приписать исходный сбой этой конкретной границе.

## Native correction before merge

First required run43051934156: macOS succeeded (install16), Windows failed
only the new held-open regression with PermissionDenied5 at TempPath::persist;
all14 prior Windows installer tests passed. Keeping a readable File handle
across replacement was an invalid portability assumption, not the original
SIGKILL or a flaky success threshold. The regression now retains old file
identity through a hard link without an open handle during rename. It still
fails on copy-through and passes on replacement; no cfg skip, retry or
production/helper change. The failed run remains part of the PR evidence;
required checks run again on the corrected commit.
