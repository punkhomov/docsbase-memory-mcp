# CF5b1: изолированный эксперимент control journal

2026-11-13, Linux x86_64 sandbox, Rust1.100.0 stable. Исходник:
[control_journal.rs](control_journal.rs), schema:
[control-state-v1.sql](../control-state-v1.sql). Product code/Cargo.toml/Cargo.lock
не меняются. Это research harness, не безопасный control-store implementation.

Используется та же **rusqlite0.40.2 bundled**, что в repository lockfile;
SELECT sqlite_version() фактически вернул **3.53.2**. Не используется Python/system
SQLite. Исходный Cargo.lock копируется только в scratch для сохранения разрешённых
transitive versions; Cargo добавляет scratch package и удаляет неиспользуемые
packages только из этой копии. Новых dependency решений для продукта нет.

Из repository root, с установленной1.100.0 и уже доступными locked dependencies:

```sh
cf5_scratch=$(mktemp -d)
mkdir "$cf5_scratch/src" "$cf5_scratch/runs"
cp Cargo.lock "$cf5_scratch/Cargo.lock"
cp docs/specs/component-configuration/experiments/control_journal.rs "$cf5_scratch/src/lib.rs"
cp docs/specs/component-configuration/control-state-v1.sql "$cf5_scratch/src/control-state-v1.sql"
cat > "$cf5_scratch/Cargo.toml" <<'TOML'
[package]
name = "docsbase-control-experiment"
version = "0.0.0"
edition = "2024"
rust-version = "1.100.0"

[dev-dependencies]
rusqlite = { version = "=0.40.2", features = ["bundled"] }
TOML
CF5B1_SCRATCH="$cf5_scratch/runs" CARGO_TARGET_DIR="$cf5_scratch/target" \
  cargo +1.100.0 test --offline --manifest-path "$cf5_scratch/Cargo.toml" -- --nocapture
```

Scratch root должен быть private/disposable. Harness создаёт новые child dirs и
никогда не удаляет существующий путь; повторный запуск использует новый process ID.
Только синтетические нулевые images. Fixture roots/parent IDs/hashes — placeholders:
тест не утверждает semantic validation, secret permission или path admission.
Полноценные native product tests и integration всех callers — CF5b2/3/5c.

| Проверка | Фактическое свидетельство Linux |
|---|---|
| SQLite PRAGMA values | DELETE; synchronous3; application_id1145193286; user_version1; foreign_keys1; page_size4096; max_page_count16384 |
| SQL refusal | Нельзя пропустить applying, изменить sealed policy/images, unseal, удалить namespace/нужные images или restore после strict |
| INSERT OR REPLACE | Два отдельных RED без insert guards; после guards оба replacement отказывают с recursive_triggers=OFF, namespace/strict policy остаются прежними |
| Process exit перед commit | 257source rows; tiny8KiB cache провоцирует spill; существует journal с ненулевым hot header; child exit91 без destructors |
| Read-only query на hot journal | SQLITE_READONLY_ROLLBACK extended776; DB/journal bytes идентичны до/после |
| Явный writable recovery | Восстанавливаются preparing и257pending rows, hot journal удаляется |
| Process exit после commit | Child exit92 без connection Drop; видны applying и257new_verified rows |
| Restore/retry journal boundary | restored_legacy terminal для старой операции; rollback замены journal rows сохраняет прежний restored record |
| Native session lock | Shared/shared допускается; exclusive при shared и shared/exclusive при exclusive отказывают; после release exclusive допускается; lockfile сохранён |
| WAL registry: отклонённая SQL reservation | Реальный WAL-format DB; BEGIN IMMEDIATE создаёт WAL/SHM до первого INSERT. Поэтому этот способ не соответствует pre-stale allowlist |
| WAL registry: выбранная native reservation | Native SHARED→EXCLUSIVE xLock одного immutable descriptor; writer subprocess получает Busy до и после read; sidecars не возникают, DB bytes прежние после release; после release writer succeeds |

Tiny cache — stress seam только эксперимента. Research contract задаёт256KiB;
эксперимент не меняет runtime defaults. Восемь top-level tests на Linux включают
один helper, который без child env завершается без работы; семь substantive tests используют
реальную DB/native locks, а не mock desired behavior. Nested child summaries
не добавляются к top-level count.

Первый запуск:5passed/0failed/0ignored; hot-journal extended776 и SQLite3.53.2.
После SQL schema corrections и fresh review выявлен неприемлемый BEGIN IMMEDIATE
boundary. Добавлен реальный WAL experiment:6passed/0failed/0ignored, Clippy clean.
Final review выявил обход DELETE triggers через INSERT OR REPLACE. Оба отдельных
regressions RED до добавления BEFORE INSERT guards, затем полный scratch suite
8passed/0failed/0ignored. Native primitive — исследовательская Linux-only выжимка уже
существующего `src/store/registry_inspect/lease.rs`; full namespace/VFS/platform
proof остаётся у owning module и следующих product gates. Заимствованные FFI
pointers не переживают private connection; unsafe blocks имеют SAFETY comments.
Во время POSIX lease harness не открывает второй DB descriptor для byte snapshot.
Проверка `fs::read` выполняется до acquisition и после connection drop.
Локальные raw logs: `/tmp/cf5b1-prototype-first.log`,
`/tmp/cf5b1-prototype-wal.log`, `/tmp/cf5b1-prototype-wal-clippy.log`,
`/tmp/cf5b1-replace-red.log`, `/tmp/cf5b1-replace-green.log`; scratch:
`/tmp/docsbase-control-prototype`. Эти пути — только session evidence;
воспроизводимый код/SQL и точные команды включены в PR.

Процесс был завершён без Rust destructors, а не потерей питания/ядра/disk cache.
Эти результаты не доказывают power-failure durability, directory fsync/Windows
replacement semantics, VFS namespace stability, bounded semantic decoder или
весь session lifetime. Linux experiment не является Windows/macOS proof.
Никаких source replacements, provider calls, model downloads, новых product
tests/ignores или изменений retrieval baseline в этом этапе нет.
