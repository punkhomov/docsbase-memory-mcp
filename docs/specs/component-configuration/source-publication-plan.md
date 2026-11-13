# CF5b2a: native publication investigation

2026-11-13. Подэтап CF5b2 после [control contract](control-contract.md).
Goal: проверить конкретные ограничения native source replacement до product API.
Это isolated research, не завершение CF5b2/CF5b3 и не activation.

1. Сопоставить POSIX rename/fsync, Apple full-sync и Windows ReplaceFile/flush.
   Указывать первичные источники и то, чего они не доказывают.
2. На Linux с existing pinned libc в scratch воспроизвести потерю target xattrs
   при простом rename; проверить частный positive case переноса одного user xattr.
   Он не заменяет полную enumeration/ACL/owner/flags реализацию.
3. Реальные process exit до/после rename и ошибка parent flush после rename:
   source может уже измениться при ошибке; terminal strict commit запрещён.
4. Missing-target create должен отказать при уже появившемся target; private stage
   и nofollow/hardlink cases проверяются до product source mutation API.
5. Описать journal-extension requirements для before/after attributes и native
   identities, stage intent/lifetime, unknown stages, aggregate bounds и recovery.
6. Source/reference/experiment/link audit, fresh task/final reviews, required CI.

Acceptance: воспроизводимый код/команды и actual Linux evidence, честные platform/
power-failure/namespace limitations. Product src/dependencies/Cargo.lock/test
inventory не меняются. No providers/downloads/source edits/rebuilds. Во всех
дальнейших product constructors действует обязательный session-before-capture gate.

CF5b2b затем выбирает и реализует bounded native metadata/journal extension и
platform seam, с отказом на неподдерживаемом storage. CF5b2c доказывает реальные
failure/recovery/attributes на Linux/macOS/Windows; только затем executor CF5b3.
Этот research slice не обещает Windows/macOS durable publication по Linux tests.
