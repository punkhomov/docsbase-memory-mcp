# macOS support — design

## 1. Platform seam

`cfg(unix)` уже выбирает `src/platform/unix.rs` для macOS; macOS-специфика
добавляется ветками `#[cfg(target_os = "macos")]` внутри него — без нового
модуля (границу проверяет `tests/platform_boundary.rs`).

### 1.1 Liveness (`process_alive`) — FR-2

```text
macOS:  proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &mut info, size)
        -> bytes == sizeof(proc_bsdinfo) && info.pbi_status != SZOMB(5)
Linux:  /proc/<pid>/stat, state != "Z" (как сейчас)
Прочие unix: текущая fail-open ветка (вне скоупа)
```

`SZOMB = 5` (Darwin `sys/proc.h`; libc не экспортирует константу для Apple —
локальная `const SZOMB: u32 = 5;` с комментарием; принятый ABI-риск).
`proc_pidinfo` для несуществующего pid возвращает 0 или короткий размер —
считаем мёртвым. Зомби (убитый, но не зарепленный ребёнок) — мёртв, паритет
с Linux.

Направление отказа асимметрично и осознано: Linux без `/proc` — fail-open
(«жив», другие liveness-проверки решают), macOS при любой ошибке
`proc_pidinfo` — fail-closed («мёртв»). Ложный «мёртв» на macOS дёргает
stale-cleanup/рестарт, но гасится admission-lease; ложный «жив» на Linux
безопасен (лишнее ожидание, не потеря состояния).

### 1.2 Счётчики — FR-3

- `fd_count`: `proc_pidinfo(PROC_PIDLISTFDS)` с буфером `proc_fdinfo`;
  при нехватке буфера — увеличиваем (×2, cap) и повторяем; результат =
  `bytes / sizeof(proc_fdinfo)`.
- `thread_count`: 0 на macOS (как Windows; `task_threads` — mach, вне скоупа).

### 1.3 Системные каталоги — FR-4

macOS-ветка `is_system_dir` (канонические пути):

- SUBTREES: `/System`, `/Library`, `/Applications`, `/private/etc`.
- EQUALS: `/Users`, `/Volumes`, `/private/var`, `/private/tmp`.
- `/etc` → `/private/etc` (canonicalize) блокируется; `/var/folders/...`
  (temp тестов) остаётся валидным корнем.
- `/opt` и `/cores` не блокируются (осознанно): `/opt` — паритет с Linux
  (пользовательский софт, включая Homebrew-префикс), `/cores` — дампы ядра,
  не корень проекта.

### 1.4 Case handling — FR-5

`normalize_for_compare` получает macOS-ветку (lowercase), как Windows:
default APFS регистронезависим. Ограничение: на case-sensitive APFS
сравнение может ложно совпасть — документируется, не блокер v1.

### 1.5 Socket guard — FR-6

`bind`/`bind_blocking`/`connect_blocking`/`connect_probe` проверяют длину
пути до вызова: лимит = `sun_path` − 1 (NUL): macOS 104 → 103, Linux 108 →
107; иначе `io::Error::new(InvalidInput, "socket path is N bytes; limit …")`.
Дефолтные пути (cache/state/daemon.sock) укладываются с запасом.

## 2. CI — FR-7

`ci.yml` получает джобу `macos` (macos-latest, `RUSTUP_TOOLCHAIN=1.88.0`,
fmt + clippy + `cargo test --locked`) — копия windows-джобы. Стартует
advisory; после зелёной стабилизации — перевод в required (branch protection
+ AGENTS.md).

## 3. Release — FR-8

`release.yml` получает джобу `macos`:

1. `cargo build --release --locked` (aarch64, без static-CRT).
2. `codesign -s - --force target/release/docsbase` (ad-hoc, страховка после
   strip) + `codesign -v` (gate).
3. `tar -czf docsbase-$VERSION-macos-aarch64.tar.gz docsbase` + `.sha256`.
4. `actions/attest-build-provenance` + `upload-artifact`.
5. `publish.needs += macos`; staging glob подхватывает `.tar.gz`/`.sha256`.

SBOM — общий на релиз (существующий job), новых не требуется.

## 4. Installer/update — FR-9

- `install.sh`: принять `Darwin`; `uname -m` `arm64` → `aarch64`; ASSET =
  `docsbase-$VER-macos-aarch64.tar.gz`; `shasum -a 256` fallback уже есть.
- `src/cli/update.rs`: `asset_name` — macOS + aarch64; `sha256_file` —
  `shasum -a 256` fallback на macOS; тест asset-имени расширяется.

## 5. Tests — FR-10

- `tests/platform_process.rs`: `/proc`-чтение (`detach_sets_own_process_group`)
  — `cfg(target_os = "linux")`; `counts_are_positive` — `fd_count > 0` (macOS
  реализован), `thread_count` — только Linux.
- `tests/registry.rs`: per-OS список системных каталогов (macOS: `/System`,
  `/Library`; Linux: как сейчас).
- `tests/watcher.rs`: `FRESHNESS_BUDGET` macOS = 5 с (unix-ветка сужается до
  `all(unix, not(macos))`).
- `src/daemon/session.rs` unit-тест: проверка liveness через proc_pidinfo
  (zombie = мёртв) — проходит без изменений.
- Smoke релиза (SC-2) — шаг в release-джобе: `--version` + `index`/`search`
  на фикстуре.

## 6. ADRs

- **ADR-M1**: `libc` как `[target.'cfg(target_os = "macos")'.dependencies]`.
  Обоснование: уже в графе (fsevent-sys → libc), ноль новых крейтов; нужен для
  `proc_pidinfo`. `unsafe` — только в `src/platform/unix.rs` с SAFETY.
- **ADR-M2**: v1 — только aarch64. Обоснование: Tier 1, Intel EOL, x86_64
  Tier 2, раннеры Intel умирают; universal — отдельный этап (lipo + подпись
  после merge).
- **ADR-M3**: ad-hoc подпись, нотаризация отложена. Обоснование: curl-установка
  без карантина; нотаризация требует Apple Developer Program ($99/год) —
  триггер: браузерная дистрибуция/жалобы Gatekeeper.

## 7. Порядок и зависимости

M1 (шов) → M2 (тесты + CI) → M3 (release/installer/update) → M4 (доки).
M2 зависит от M1 (liveness/counters). M3 не зависит от M2, но CI-гейт M3 —
macOS-джоба из M2.
