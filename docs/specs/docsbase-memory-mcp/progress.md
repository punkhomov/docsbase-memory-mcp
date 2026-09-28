# docsbase-memory-mcp ledger — plan: docs/specs/docsbase-memory-mcp/tasks.md

BASE (before T1): 1e02a47

## Task log

Task 1: fix round 1/5 (1 addressed: cargo fmt; 0 open — tester: cargo test 3/3, clippy clean, fmt check OK)
Task 1: minor (deferred): smoke.rs:19-34 — config checks are substring-based, not TOML-parsed
Task 1: Ruling: NFR-6 (static binary) had no owning task — added T32 (release/static hardening); coverage updated
Task 1: complete (commits 1e02a47..a5f6265, tests: cargo test --test smoke → 3/3, clippy -D warnings → clean, fmt → clean)

Task 2: Ruling: plan defect — `serde_json::Error` conversion moved from T2 to T5 (crate appears only in T5); tasks.md updated
Task 2: minor (deferred): `From<io::Error>` maps to `Internal` (-32603), drops `ErrorKind`/source; T11/T29 must remap file-IO to `Error::Index`
Task 2: minor (deferred): Cargo.toml/Cargo.lock absent from T2 `Files` (plan oversight; Files list updated)
Task 2: complete (commits a5f6265..f70aa04, tests: cargo test → 6/6, clippy clean, fmt clean; review PASS/APPROVED)

Task 3: Ruling: dev-dep `tempfile` moved T4→T3 (config tests need temp dirs); plan updated
Task 3: Ruling: design §2 "один крейт на задачу" противоречил конституции — приведён к правилу «крейты из §7 с обоснованием»
Task 3: minor (deferred): CLI-precedence — opt-in `with_overrides`, вызывающий может её пропустить (src/config/mod.rs:97)
Task 3: minor (deferred): paths::cache_dir/config_dir не покрыты юнит-тестами; проверить через DOCSBASE_* env в T17/T19
Task 3: minor (deferred): ошибки чтения/парсинга конфига маппятся в Internal (-32603); T29 должен дать actionable категорию
Task 3: complete (commits f70aa04..3567290, tests: cargo test → 11/11, clippy clean, fmt clean; review PASS/APPROVED)

Task 4: Ruling: review Med (cache root umask) — fixed via chmod 0700 + regression test; fix commit d685040
Task 4: minor (deferred): migrate/read_version messages lack DB path; wrap in T11
Task 4: minor (deferred): FK cascade + UNIQUE constraints untested; cover in T11/T21
Task 4: minor (deferred): ProjectStatus::as_str/parse round-trip untested
Task 4: complete (commits 3567290..d685040, tests: store 6/6, clippy clean, fmt clean; review PASS + fix ADDRESSED)

Task 5: Ruling: T5 `Files` дополнены `src/error.rs` — сюда перенесена отложенная из T2 конверсия `From<serde_json::Error>`
Task 5: minor (deferred): только `Response::Hello` в roundtrip; нет raw wire fixture и `\r`-кейса (tests/ipc_protocol.rs)
Task 5: minor (deferred): wire использует i32/u64 вместо ErrorCode/usize из эскиза design §5 — обновить эскиз или зафиксировать отклонение
Task 5: minor (deferred): асимметричный API (`encode` generic, только `decode_request`); T20 добавит декод ответов
Task 5: complete (commits d685040..7d8a8f4, tests: cargo test → 21/21, clippy clean, fmt clean; review PASS/APPROVED)

Task 6: Ruling: `walk` сигнатура — `Result<impl Iterator>` (eager-валидация `..`), а не голый iterator из плана
Task 6: fix: `.git` добавлен в `DEFAULT_IGNORED_DIRS`; тесты на hidden/dist/build и `resolve_in_root` (review Low #1)
Task 6: minor (deferred): вложенные `.docsbaseignore` с `..` не валидируются (escape невозможен); пост-фильтрация default-папок — перф-долг в T30
Task 6: complete (commits 7d8a8f4..ca85ec8+fix 290363b, tests: walk 7/7, clippy clean, fmt clean; review PASS/APPROVED)

Task 7: minor (deferred): unquote снимает непарные кавычки (`'Twas` → `Twas`); `--- ` с хвостовым пробелом не распознаётся как закрытие (frontmatter.rs:83,32)
Task 7: minor (deferred): нет регрессионных тестов для `"---"`, `"---\n---"`, без trailing newline, multibyte body
Task 7: complete (commits 290363b..ee1dacb, tests: frontmatter 5/5, clippy clean, fmt clean; review PASS/APPROVED)

