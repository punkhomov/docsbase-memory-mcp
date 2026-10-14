# macOS support — progress ledger

Plan: `docs/specs/macos-support/tasks.md` · Spec: `docs/specs/macos-support/requirements.md`
Machine mode: fresh reviewer subagent per task; ledger appended with each task commit.

Plan created: M1 (шов: liveness/counters/paths/socket guard) → M2 (тесты + CI-джоба macos) → M3 (release/installer/update) → M4 (доки/конституция).
Decisions: aarch64-only (ADR-M2), min macOS 12, ad-hoc подпись + `codesign -v`, нотаризация отложена (ADR-M3), `libc` как macOS target-dep (ADR-M1, уже в графе транзитивно).
Task M1: complete (commits f6d5c73 + review fixes afffb47/a4c1262/9702e7f + this round; macOS CI compile+unit green; Linux 366/0/8)
Task M1: Ruling: liveness — proc_pidinfo(PROC_PIDTBSDINFO) + pbi_status != SZOMB(5) (зомби = мёртв, паритет с Linux "Z"); fd_count — PROC_PIDLISTFDS с bounded-ростом буфера; thread_count = 0 (как Windows); socket guard — len >= sun_path (104/108, NUL включён) → InvalidInput с подсказкой.
Task M1: Ruling: case-fold macOS в normalize_for_compare и is_under (default APFS case-insensitive; caveat для case-sensitive томов задокументирован); system dirs — SUBTREES /System,/Library,/Applications,/private/etc,/dev; EQUALS /Users,/Volumes,/private/var,/private/tmp (temp-каталоги тестов остаются валидными корнями).
Task M1: Review: spec PASS/quality APPROVED; minors закрыты (zombie-тест без reap, macOS fold-тесты, is_under macOS-ветка); Important перенесён в M3 — sha256_file без shasum-fallback (на стоковом macOS нет GNU sha256sum) — гейт M3.
Task M2: complete (commits 55a15cb..9702e7f; macOS CI зелёный на полном функциональном сьюте — run 38443828761 job 115456932878; Linux 366/0/8; Windows green)
Task M2: Ruling: macOS-открытия CI-итераций — (1) unsafe_code lint → #[allow] + SAFETY (паттерн windows.rs); (2) BSD accept наследует O_NONBLOCK → BlockingStream::set_nonblocking + сброс в фейке; (3) XNU sosetoptlock возвращает EINVAL для setsockopt на shut-down сокете → фейк дропает мёртвые probe-коннекции; (4) flood-тест — явный 60s grace + bounded wait на connect+handshake.
Task M2: Review: spec PASS/quality APPROVED; minors — connect+handshake retry закрыт, тихий дроп мёртвых probe-коннекций принят (задокументирован), stale-имя watcher-теста принято (паттерн Windows).
Task M3: complete (commits cd500ed..5bc965b; release workflow dispatch 38450546421: macos job success — build, codesign -v, smoke install/index/search, package, attest)
Task M3: Ruling: релиз — ad-hoc подпись `codesign -s - --force` + gate `codesign -v` (Apple Silicon убивает неподписанные бинари); нотаризация отложена (ADR-M3, триггер — браузерная дистрибуция); `.sha256` через `shasum` (формат совместим с `shasum -a 256 -c`); smoke теперь гоняет бинарь из распакованного тарбола (round-trip) и ассертит `uname -m = arm64`.
Task M3: Review: spec PASS/quality APPROVED; minors закрыты (round-trip smoke, arch-assert, док-синк header/update.rs).
Task M4: complete (commit 3c76932 + review round; docs CI gate зелёный)
Task M4: Ruling: платформенные заявления — macOS 12+ Apple Silicon (Intel/universal out, ADR-M2); install-путь macOS ad-hoc + curl без карантина; конституция — delta-маркер «артефакты Linux + Windows + macOS aarch64»; AGENTS — macOS-грабли (exclusive cfg, SZOMB, O_NONBLOCK/XNU EINVAL, codesign) и advisory-статус macos-джобы.
Task M4: Review: spec PASS/quality APPROVED; minors закрыты (ARM64 Windows в out-of-scope, macOS-верификация sha, macOS-пути data/config).
