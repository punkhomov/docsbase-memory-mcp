# CF6c: срок аренды ANN и актуальность статуса

Исполнение inline по superpowers:executing-plans, без делегирования реализации.
Свежие task/final review обязательны по AGENTS.md. База: PR137 f027b38.
Rust1.100.0 stable; без новых зависимостей, baseline/threshold changes, API-вызовов
и загрузки моделей.

## Контракт и альтернативы

Внедрить I35 при создании компонентов, без новых source keys, apply или live shrink.
DenseSpec владеет обоими прежними часовыми defaults. Проверенный immutable
AnnLifetimes входит в DenseSettings/DenseCacheLimits и проходит через существующие
явные CLI/lifecycle/Shared/sync пути. Закрытые поля не позволяют передать невалидный
TTL в прежние infallible-конструкторы raw limits. Новых runtime defaults и locks нет.
Отдельный SettingsSpec/default catalog и повторное разрешение policy в job отвергнуты.

Backfill TTL — положительное целое число секунд, представимое в i64 для SQLite.
Дробные значения отклоняются, а не округляются до нуля. Прежняя saturating-арифметика
expiration сохраняется. Outcome TTL — положительный Duration, возраст измеряется
монотонно. Настройки не отзывают старые leases и не меняют vector/query/ANN identities.
Первичный pin и успешное продление используют один переданный TTL; отказ удаляет
только соответствующие project/model/hash lease keys. Текст stale reason описывает
настроенный TTL вместо неверного утверждения об одном часе. State names и часовые
переходы по умолчанию сохраняются. is_stale()/ANN_OUTCOME_TTL остаются adapters.

## Интерфейсы

- dense_settings::AnnLifetimes::new(backfill_pin_ttl: Duration,
  outcome_ttl: Duration) -> Result<Self>; закрытые поля и Duration getters.
- DenseSettings / DenseCacheLimits: ann_lifetimes: AnnLifetimes.
- DensePatch: optional backfill_pin_ttl / ann_outcome_ttl; DenseSpec владеет
  daemon-scope полями, проверкой и отдельным provenance в transactional resolution.
- AnnOutcome::is_stale_after(ttl: Duration) -> bool; daemon передаёт
  dense.limits().ann_lifetimes.outcome_ttl().
- Backfill with_dense_policy использует переданный pin TTL в обоих pin calls.

## Task CF6c

- [x] API compile RED; реальные consumer RED для initial pin, renewal и status.
- [x] Проверки zero/fractional/overflow pin TTL и zero outcome TTL; независимые
  overlays/provenance, project-scope rejection с валидным TTL и rollback invalid patch.
- [x] Реальные indexed project/vector SQL: границы первого expiry, укорочение аренды
  fake embedder и правильное продление; ошибки сохраняют leases другого проекта
  той же модели и другой модели того же проекта. Без sleep/clock override.
- [x] Настоящий status/metadata: после продвижения монотонных часов за 1ns TTL
  custom cache становится stale, control с прежним default остаётся ready.
- [x] Owner type/defaults, projection, jobs/status; усиленные Shared/direct CLI tests.
- [x] Полный cli-full, lean focused, full/lean Clippy/fmt/diff и dated deltas.
- [x] Fresh task review без замечаний; task ledger commit.
- [ ] Fresh final review, required native CI/inventory audit и разрешённый squash
  merge. Остальные CF6/CF7/CF8/#78 и live policy updates не объявляются завершёнными.
