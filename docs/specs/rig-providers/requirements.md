# Rig providers — первый этап

Дата: 2026-11-12. Tracking: #128.

## Цель и границы

Внедрить Rig в существующий OpenRouter embeddings/rerank путь и эмпирически
проверить совместимость. Это законченный первый этап миграции провайдеров;
генерация, новые провайдеры, ask_docs и async orchestration остаются в #128/#81.

- FR-1: единственный runtime путь OpenRouter использует кодеки Rig; без нового
  пользовательского режима, флага или параллельного реестра.
- FR-2: сохранить model/cache identity, query/document роли, dimensions,
  batching, normalization, top_n, сортировку и атрибуционные заголовки.
- FR-3: до записи в cache проверить количество, размерности, диапазон и
  уникальность индексов. Ответ без индексов означает порядок входа; смешанный
  ответ запрещён. Нечисловые/непредставимые f32 значения запрещены.
- FR-4: сохранить transport seam, timeout, retry/status mapping, blocking guard
  и отсутствие скрытого fallback. Не создавать новый runtime/retry слой.
- FR-5: FastEmbed остаётся в том же Provider/Stages механизме с прежним
  provisioning и cache. Не подключать второй FastEmbed/ORT.
- FR-6: Cargo default и hybrid-local не включают Rig или remote HTTP stack.
  Новые зависимости проходят 14-дневный cooldown, MSRV согласован с CI.
- FR-7: задокументировать проверенные несовместимости, ограничения этапа и
  следующие шаги; не объявлять поддержку непроверенных remote моделей.

## Приёмка

- SC-1: fixture/contract tests проверяют FR-1–FR-4, включая переставленные
  индексы, отсутствие usage, неверные индексы/размерности и finite f32.
- SC-2: default dependency tree не содержит Rig/reqwest; проверены local и
  cli-full сборки, golden search без изменения baseline.
- SC-3: fmt, clippy, функциональные проверки и независимый review; native CI
  проверяется отдельно от локального Linux результата. Live вызовы не обязательны.
