# Результат ограниченного эксперимента — 2026-11-17

**Вывод:** привязать учёт ресурса к существующему native owner Mio и получить
барьер завершения записи удалось небольшими патчами Mio/Tokio. Необходимость
собственного IOCP для этих двух механизмов не доказана. Готовность библиотечного
варианта заменить весь PR #164 этим экспериментом не установлена.

Эксперимент закончен. Его ветка `experiment/windows-mio-retirement` не предназначена
для merge; production PR #164 остаётся без изменений на `5982286`. Патчи зависимостей
не предлагаются к поставке в текущем виде.

## Проверяемый источник и нативные результаты

- Tested commit: `73226fc91cd618947c4049240b37ff405cb42758`.
- Tested tree: `aeb8833c9344ca0093e828bb6bda3fff20744f5e`.
- [Windows run 44144642865](https://github.com/punkhomov/docsbase-memory-mcp/actions/runs/44144642865):
  Windows Server 2025, `x86_64-pc-windows-msvc`, Rust 1.100.0; success.
- Executable SHA256:
  `b95b3bfd5bc1d87c52540cec413bb92d0dea88acc8674adc2a3519345d06cb61`.
- `mio.patch` SHA256:
  `5a31a3b35649feba31ec94a2e05d9a31008d55bb5b2a3f54fd20a51525a25fad`.
- `tokio.patch` SHA256:
  `8a4c21e9ee40e21a3cc44b8bdd79325e2ecc7780448a955b09d3c866604345ea`.

Артефакт run содержит исходный HEAD/tree/toolchain, нормализованные хэши патчей,
хэш executable, stdout/stderr и exit status каждого процесса. Они сверены с git
objects. Убийства watchdog и принудительной очистки не было. Архивы зависимостей
проверены по checksum существующего root Cargo.lock; новых registry-пакетов нет.
Артефакт хранится 14 дней; существенные наблюдения сохранены здесь.

| Проверка | Наблюдение |
| --- | --- |
| Sensitivity RED: guard на внешнем stream | `pending=true held=0 available=1`; затем `cleanup_live=0 cleanup_slots=1`; ожидаемый assertion, exit 101 |
| Guard внутри Mio | `pending=true held=1 available=0`; после полного чтения `cleanup_live=0 cleanup_slots=1` |
| Persistent write barrier | 8 × 1 MiB, flush ожидает нативную запись, точные байты, drain helpers = 0 |
| Disconnect и повторное admission | 16 failed-exchange циклов при cap = 1; исходный слот удерживается и затем возвращается |
| Control progress | Другая принятая pipe передаёт полный control reply, пока dirty pipe удерживает свой слот |
| Error/unwind и внешний runtime | Учёт переживает logical Drop и последующую ошибку/panic; сохранённый runtime завершает очистку |
| Успешный final drain | 8192 точных байта; отдельный FlushFileBuffers worker joined до освобождения удерживаемого owner |

GREEN: **6 passed, 0 failed, 0 ignored**, exit 0. Panic в GREEN stderr намеренно
перехвачен тестом unwind. RED меняет только место guard при тех же patches;
это не сравнение со всем непатченным main/interprocess.

Первый run 44143640397 остановился до компиляции: Windows checkout перевёл patches
в CRLF, а crate sources остались LF. RED/GREEN на CRLF-входе подтвердил причину.
`prepare.py` теперь нормализует patch stdin; проверена побайтовая идентичность
прежним reviewed library sources. Нативное поведение hooks не менялось.

## Что остаётся недоказанным

- Это raw Tokio pipes, без interprocess и реального daemon pipeline. Нет проверки
  настоящих per-request Activity tails или общего process-level resource cap;
  тестовые peer handles не входят в admission приложения.
- Runtime намеренно сохранён живым снаружи. Уничтожение selector/runtime не
  гарантирует обработку будущих completion; root/shutdown интеграция обязательна.
- В unwind-тесте server явно dropped **до** panic. Очистка, инициированная самим
  unwind живого server, не проверена.
- Final-drain тест проверяет успешный join, не bounded/cancellable helper.
  Отмена до входа в FlushFileBuffers, запоздалый cancel caller, блокирующие
  клиенты и их deadlines требуют отдельной production-проверки.
- Сравнения CPU, RSS, стека, latency или throughput с PR #164 нет. Длительность
  шести unit cases не является performance benchmark.
- Fresh reviewer обнаружил унаследованную условную утечку Arc: в patched Mio
  `named_pipe.rs:839` synchronous continuation забывает дополнительный Arc после
  retain в `maybe_schedule_write:809`; completion восстанавливает один. Достижимость
  при наших byte-mode PIPE_WAIT настройках не установлена и этими тестами не
  форсируется. Это **Important unresolved adoption obligation**, а не
  воспроизведённое падение эксперимента.
- Minor (deferred): экспериментальная документация Tokio constructor неточно
  объединяет registration errors и отсутствие runtime; последнее может panic.

## Практическое решение

Рекомендация — оставить #164 draft и предпочесть дальнейшую проверку библиотечной
интеграции на существующем реальном daemon regression, прежде чем принимать
обязательство поддерживать собственный native backend. Это приоритет следующего
production-этапа, не решение поставить экспериментальные forks.

Два patches содержат 89 добавленных и 7 удалённых строк, но размер этих hooks
нельзя сравнивать с размером целого #164: application ownership, shutdown,
Activity, helper/deadline policy и review остаются работой проекта в обоих
вариантах. Forks двух зависимостей тоже требуют сопровождения; upstream merge
не обещан. Выбор для поставки должен учитывать полный объём оставшейся интеграции
и закрыть перечисленные lifecycle obligations при сохранении существующих
проверок. Никаких baseline, thresholds, production budgets или required gates
эксперимент не меняет.

Task MIO-PROBE: complete (2026-11-17, bounded native hook feasibility).
Ruling: fresh independent reviewer — converged for the experiment; whole-production
replacement unverified. No introduced Critical/Important finding in the hooks or
CRLF harness delta; inherited conditional leak remains unresolved before adoption.
