# План расследования #109 — 2026-11-11

Исполнение: `superpowers:executing-plans`, последовательно в этой сессии.

**Цель:** получить переносимое свидетельство нестабильной recall на macOS,
не подменяя расследование изменением production HNSW-параметров.

**Исходное состояние:** main `2cfec2a`; Linux stress пока не воспроизвёл
исходный macOS recall=0.945. Гонка первого entry point и связность графа —
гипотезы, не установленные причины.

**Ограничения:** Rust1.88, существующие зависимости, официальный cli-full;
никаких изменений threshold0.99, baseline, tokenizer, runtime settings,
daemon/config/server, persisted production identity либо paid/live API.
Required docs/linux/macos/windows сохраняются. PR ссылается на #109, но не
закрывает её без установленной причины и проверенного исправления.

## T24 — сохранить и повторить failing graph

Файлы: `tests/ann_recall.rs`, новый test-only helper
`tests/support/ann_capture.rs`, manual-only workflow
`.github/workflows/ann-investigation.yml`, dated deltas в ann-index и
отчёт `reports/recall-investigation-2026-11-11.md`.

- [x] Сохранить существующую проверку100 queries ×10 hits и recall>=0.99.
  При short results/quality failure перед assert записать actual query
  vectors, actual hit IDs/scores, platform/thread context и именно построенный
  граф через существующий `AnnIndex::dump`/`AnnStore`.
- [x] Проверить capture/reload на настоящем маленьком графе: после drop
  исходного AnnIndex loaded graph возвращает сохранённые IDs/scores.
- [x] Добавить явно ignored replay test с обязательным path к bundle;
  проверять исходную quality guarantee, не rebuild графа. Нормальный suite
  не должен читать произвольные прежние артефакты.
- [x] Использовать уже загружаемый `target/ci-observations` для failing bundle.
  Manual macOS stress запускает неизменённую ann_recall suite несколько раз,
  останавливается с failure при первом сбое; никакого retry-until-green.
- [ ] Локально выполнить ann_recall/helper/replay, fmt/clippy и unchanged
  metrics_match_baseline; проверить manual native evidence. Затем fresh task
  review, required CI на финальном HEAD и final review.
- [ ] Если capture воспроизведёт дефект, расследовать именно этот граф и
  согласовать следующий небольшой fix contract в dated delta. Иначе оставить
  #109 открытой и явно записать ограничение доказательств.

**Публичные интерфейсы:** отсутствуют. Helper и replay живут исключительно
в integration tests. Успешный штатный test не пишет новые diagnostics.
