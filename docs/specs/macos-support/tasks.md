# macOS support — tasks

## M1 — Платформенный шов

**Goal:** macOS-ветки в `src/platform/unix.rs` и `paths.rs`: liveness (FR-2),
счётчики (FR-3), системные каталоги (FR-4), case-fold (FR-5), socket guard
(FR-6).
**Files:**
- Modify: `Cargo.toml` (libc как macOS target-dep, ADR-M1)
- Modify: `src/platform/unix.rs` (process_alive/fd_count/thread_count/guard)
- Modify: `src/platform/paths.rs` (macOS-списки, normalize_for_compare)
- Test: `tests/platform_process.rs` (Linux-гейты), `tests/registry.rs`
- Verify: полный сьют на Linux без регрессий; macOS-джоба CI (M2) — компиляция
**Steps:**
1. ADR-M1: `libc` target-dep; SAFETY-комментарии.
2. `process_alive` macOS: `proc_pidinfo(PROC_PIDTBSDINFO)` + SZOMB.
3. `fd_count` macOS: `PROC_PIDLISTFDS`; `thread_count` = 0 (док-комментарий).
4. `is_system_dir` macOS-ветка; `normalize_for_compare` fold.
5. Socket guard в bind/connect (+ Linux-лимит 107).
6. Linux-сьют зелёный; коммит.
**Acceptance:** FR-2…FR-6; Linux/Windows без изменений поведения.
**Verify:** `cargo test --locked` (Linux) → ok; macOS-джоба — компиляция + тесты.

## M2 — Тесты и CI-джоба

**Goal:** macOS-гейты тестов (FR-10) + джоба `macos` в CI (FR-7) до зелёного.
**Files:**
- Modify: `.github/workflows/ci.yml` (джоба macos)
- Modify: `tests/platform_process.rs`, `tests/registry.rs`, `tests/watcher.rs`
- Modify: `src/daemon/session.rs` (unit-тест при необходимости)
**Steps:**
1. Linux-гейты `/proc`-тестов; per-OS system-dirs; macOS FRESHNESS_BUDGET=5s.
2. Добавить джобу macos (копия windows).
3. Итерации по логам macOS CI до зелёного функционального сьюта.
4. Коммит; решение о required-check — отдельно (после стабилизации).
**Acceptance:** SC-1, SC-3; Linux/Windows без регрессий.
**Verify:** CI: docs/linux/windows/macos зелёные.

## M3 — Release, installer, update

**Goal:** macOS-артефакт (FR-8) + install.sh/update.rs (FR-9) + smoke (SC-2).
**Files:**
- Modify: `.github/workflows/release.yml` (джоба macos, publish.needs)
- Modify: `install.sh` (Darwin/arm64)
- Modify: `src/cli/update.rs` (asset_name, shasum fallback, тест)
**Steps:**
1. release-джоба: build → codesign -s - --force → codesign -v → tar.gz →
   sha256 → attest → upload; smoke (--version/install/index/search).
2. install.sh: Darwin-ветка + выбор ассета.
3. update.rs: macOS/aarch64 + shasum fallback + тесты.
4. Dry-run release-джобы на ветке (workflow_dispatch) — артефакт и smoke.
**Acceptance:** FR-8/FR-9; SC-2.
**Verify:** release workflow (dispatch) на ветке; `update` unit-тесты.

## M4 — Доки и конституция

**Goal:** FR-11: README(.ru), INSTALL, QUICKSTART, ARCHITECTURE, SECURITY,
CHANGELOG, delta-маркер конституции, AGENTS, issue-template.
**Files:**
- Modify: `README.md`, `README.ru.md`, `docs/INSTALL.md`, `docs/QUICKSTART.md`,
  `docs/ARCHITECTURE.md`, `SECURITY.md`, `CHANGELOG.md`,
  `docs/specs/constitution.md` (delta), `AGENTS.md`,
  `.github/ISSUE_TEMPLATE/feature_request.md`
**Steps:**
1. Платформенные заявления: macOS поддерживается (aarch64, 12+); asset names;
   установка; Gatekeeper/карантин-заметка; нотаризация — out of scope.
2. Конституция: delta-маркер «артефакты: Linux, Windows, macOS (aarch64)».
3. AGENTS: required-checks (после перевода macOS в required).
4. Коммит.
**Acceptance:** FR-11; docs-гейт CI зелёный.
**Verify:** `cargo test --locked --test smoke` (docs) + CI docs.
