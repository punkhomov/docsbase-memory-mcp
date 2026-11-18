# Библиотечный Windows transport — план интеграции, 2026-11-17

Goal: исправить #155 в реальном daemon path, используя existing Mio/Tokio native
ownership вместо собственного IOCP. Сохранить NDJSON, admission/Activity,
успешную финальную доставку, failed-exchange abort и существующие deadlines.

Architecture: существующие application scopes/clients из PR164 сохраняются.
Один transport-owned Tokio runtime (один I/O worker с явным stack budget) живёт
независимо от application runtime до физического retirement. Mio Inner удерживает
последним полем guard исходного handle token; bounded arena хранит application
charges и helper joins, но не kernel operations. Tokio pipe flush ждёт native
write completion; final peer drain остаётся отдельным bounded helper.

Используются using-git-worktrees, writing-plans и executing-plans. Исполнение
в этой сессии, свежие reviewers только на gates. ADR ниже обосновывает patches.

## Binding constraints

- Rust1.100.0, official CLI --features cli-full; lean остаётся offline.
- No unwrap/expect/panic in production src; unsafe только с SAFETY proof.
- Не менять baseline, thresholds, ignored tests, deadlines и production grace.
- Нельзя освобождать handle/buffer/permit/Activity по logical Drop или одному
  результату CancelIoEx. Сохранить runtime до всех native guards и helper joins.
- PR164 остаётся frozen5982286 до принятия replacement. Только PR; human merge.

## L1 — реальные библиотечные owners вместо IOCP

Files: vendor/{mio,tokio}, vendor/patches, Cargo.toml/lock,
src/platform/windows/{native_io,connection,root,flush,transport}.rs.

- [x] RED: сохранить реальный stalled_get_doc regression; mutation attachment
  должна падать только после cleanup. Существующие final-delivery, deadlines,
  cancel/helper и root cases портируются без ослабления гарантий.
- [x] Зафиксировать исходники existing locked crates, checksums и exact patches;
  guard last-field, native flush, Tokio forwarding; устранить redundant Arc retain.
- [x] Заменить Operation/OVERLAPPED/IOCP код библиотечными endpoints. Reserve до
  HANDLE creation; registration под transport runtime; guard до first I/O.
- [x] Native cancellation caller и drain helper удерживают strong endpoint owner
  до возврата/join. Abort закрывает logical endpoint, arena держит only accounting;
  исключить циклы arena → endpoint → guard → arena.
- [x] Проверить bounded buffer capacity (Mio read buffer тоже часть envelope),
  listening/admitted/rejected instances и повторное использование исходных slots.
- [x] Windows-target check/clippy, native real-daemon RED/GREEN, fresh task review.

## L2 — production acceptance и завершение PR

- [x] Native persistent/final delivery, repeated cancellation, held caller/control,
  blocking deadlines, root error/unwind; whole CLI/daemon/Node composition checks.
- [x] Проверить происхождение vendored patches в docs CI, включая original age
  records: local source не является обходом Cargo supply-chain gate.
- [ ] Full lean/cli-full, clippy/fmt, existing quality/offline gates и required
  docs/linux/macos/windows без weakening. Picomatch blocker166 отдельно видим.
- [ ] Обновить specs/ledger/PR body, independent final review, stop ready for human.

## ADR-WLIB1 — локально проверяемые patches существующих зависимостей

Status: selected for implementation, pending production acceptance. Новых crates,
версий или runtime services нет: Mio1.2.4 и Tokio1.54.1 уже есть в root lock.
[patch.crates-io] с vendored exact crate sources обеспечивает offline builds,
воспроизводимость и отсутствие зависимости от изменяемой remote branch.
Полные upstream sources/license сохраняются; patch files отдельно показывают
собственную область сопровождения. Upstream acceptance не обещан. Перед сменой
версий обязательно reapply/review + native acceptance; checksum/original age gate
не обходится local source. Альтернативы: own IOCP из164 несёт существенно больше
native lifecycle кода; unpatched libraries не дают физический guard/write barrier.
Размер vendored source не является размером собственного патча или доказательством
меньшей production complexity. CPU/RSS/latency improvement не заявляется.

Delta 2026-11-18: L1 implemented and independently reviewed; native source7e106a4
passes21ownership scenarios, two meaningful mutation REDs and the controlled
submitted-connect classification probe (run44267931680). L2 full composition
and required PR checks remain separate acceptance gates; advisory166 blocks the
mandatory native jobs. No production readiness or merge is implied by L1.
