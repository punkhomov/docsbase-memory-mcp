# Git и docsbase-memory-mcp — исследование

> Дата: 2026-09-30.
> Вопрос: как `docsbase-memory-mcp` работает с git — определение корня, worktrees, ignore-слои,
> watcher/инкремент, изоляция кэша — и что нужно менять.
> Источники: код проекта, локальные эксперименты с реальным бинарём; `docs/cbm-research.md`
> (§1.4, §7); git-документация (rev-parse/worktree/status); docs.rs `ignore` 0.4.30;
> README `DeusData/codebase-memory-mcp`; README `MikeRecognex/mcp-codebase-index`;
> context7 (gix, git, ignore); web-поиск.
> Документ — отдельный конспект; другие файлы проекта не затрагиваются.

## 0. TL;DR

- **Текущая модель** (проверено на бинаре, `/tmp/opencode/gitexp`): linked worktree — отдельный
  проект с ключом = путь worktree; `.gitignore` и `.git/info/exclude` в worktree работают;
  запуск из подкаталога поднимается до git-корня; main + linked worktree дают **два индекса**
  (дублирование содержимого).
- **Ignore-слои менять не нужно**: `ignore` 0.4.30 сам разбирает `.git`-файл (gitlink) и
  `commondir`, читает exclude из common dir linked worktree (есть апстрим-тест и локальное
  подтверждение).
- **Git-зависимостей не добавляем** (`gix`/`git2`/shell-out отклонены): наш hash-walk
  (blake3) не зависит от git и корректен для грязного/detached состояния.
- **Канонический root = worktree-root** (как сейчас); `git_common_root` — потенциальные
  метаданные (фаза 2+), не ключ индекса.
- **Containment нельзя привязывать к common dir**: сессия в linked worktree не должна читать
  docs main-worktree (это же ограничение закрывает T47).
- **Watcher (notify) достаточен**: checkout = всплеск событий, уже коалесцируется
  (`MAX_DEBOUNCE` ≤ 2 с); git-поллинг как в CBM не нужен.

## 1. Текущее состояние (код + эксперимент)

### 1.1 Код

- `registry::git_root`: walk-up до ближайшего `.git` (**файл или каталог**) → `ensure_project`
  берёт его как `root`; кэш и индекс ключуются canonical root (FR-13, I3).
- `index::walk::walk`: `WalkBuilder` с `hidden(true)`, `follow_links(false)`,
  `git_ignore(true)`, `git_exclude(true)`, `git_global(false)`, `.docsbaseignore`;
  `DEFAULT_IGNORED_DIRS` включает `.git` (FR-14, FR-32).
- Watcher: `notify` + debounce/коалесценция (`MAX_DEBOUNCE` 2 с, `is_pruned` скрывает `.git`)
  — FR-15.
- `run_full`: blake3-хэши документов, «changed/skipped/removed», git не участвует.
- Индексируются все `*.md` рабочего дерева, включая **untracked** (сознательно: агент
  работает с файлами до коммита).

### 1.2 Эксперимент (`/tmp/opencode/gitexp`, бинарь `target/debug/docsbase`)

```
$ git worktree add ../wt -b feature      # main + linked worktree
wt$ cat .git        → gitdir: /tmp/opencode/gitexp/main/.git/worktrees/wt
wt$ git rev-parse --show-toplevel --git-dir --git-common-dir
  /tmp/opencode/gitexp/wt
  /tmp/opencode/gitexp/main/.git/worktrees/wt
  /tmp/opencode/gitexp/main/.git
```

- `docsbase index .` в `wt` + `.gitignore(ignored.md)` + `.git/info/exclude(secret.md)`:
  проиндексированы ровно `a.md` и `docs/b.md`, проекта `wt`, root = `/tmp/opencode/gitexp/wt`.
  → exclude из common dir **соблюдается** (ignore crate), worktree-root = ключ.
- `docsbase index .` в `wt/docs`: root всё тот же `wt` (walk-up до git root), второй проект
  не создан.
- `docsbase index .` в `main`: создан **второй проект** `main` — идентичное содержимое
  индексируется дважды.
- `.git`-файл в walk не попадает (hidden); при `follow_links(false)` symlink-записи отсекаются.

## 2. CBM (из `docs/cbm-research.md`)

- `is_worktree = (git_dir != git_common_dir)`; `canonical_root` = корень **основного** репо,
  но это **информационное поле**, а не ключ индекса.
- Каждый worktree-root = отдельный проект/БД (имя проекта из полного пути); шаренного индекса
  и `git worktree list` нет; `Project → HAS_BRANCH → Branch` хранит branch/head_sha/base_sha.
- Watcher — **git-поллинг** (`git status` + HEAD, adaptive 5–60 с, non-git не поллятся);
  `.worktrees`/`.claude-worktrees` в always-skip; `is_worktree` входит в incremental state.
- Диагностика: `index_status(verbose)` показывает worktree/shadow Git пути; `list_projects` —
  branch.
- Полезное для нас: (а) branch/head-метаданные всё равно не влияют на retrieval; (б) главный
  их мотив — детект изменений в sandbox/passthrough сценариях, у нас его заменяет notify.

## 3. Внешние практики

- **mcp-codebase-index** (Python, AGPL): перед каждым запросом `git diff`+`git status`
  (~1–2 мс), кэш валидируется по HEAD, при ≤20 изменениях — инкремент, иначе rebuild; кэш-
  файл кладётся **в корень репо** (мы храним в `~/.cache` — чище, не мусорим в проекте).
- **git-документация**: не читать `$GIT_DIR` напрямую для refs (per-worktree HEAD,
  `commondir`, исключения `refs/bisect|worktree|rewritten`); при доступе — `rev-parse
  --git-path`; для парсинга — `git status --porcelain=v2 -z`, `git worktree list --porcelain`;
  worktree-локальный `HEAD`, общие `refs/*`.
- **gix**: `Repository::common_dir()`, `worktrees()`, `is_bare()` — если когда-нибудь понадобится
  полноценный git-клиент.
- **ignore 0.4.30**: `resolve_git_commondir` ( `.git`-файл → gitdir → `commondir` relative/
  absolute), апстрим-тест `git_info_exclude_in_linked_worktree`; precedence
  `.ignore > .gitignore > .git/info/exclude > global` (global у нас выключен).

## 4. Решения по вопросам

| # | Вопрос | Решение | Почему |
|---|---|---|---|
| Q1 | Ключ проекта для worktree | **worktree-root** (как сейчас); `git_common_root` — только метаданные позже | Нет общего индекса → нет «чужого» содержимого в containment и citations; main+wt — два независимых контекста, как у Claude Code worktree-сессий |
| Q2 | Ignore в worktree | **Ничего не менять**, закрыть регресс-тестом | ignore crate уже читает gitlink+commondir (апстрим-тест + наш эксперимент) |
| Q3 | Детект изменений | **notify остаётся**; git-поллинг не добавлять | checkout = burst файловых событий, уже коалесцируется в один батч-джоб; polling = дублирующий механизм и таймеры |
| Q4 | git-инкремент (diff/HEAD) | **Не внедрять** | blake3-walk корректен для грязного дерева/untracked; git-diff добавляет submodule/rename/quote-краевые случаи без выигрыша на docs-репо |
| Q5 | Метаданные branch/head_sha | Кандидат фазы 2 (`status`, `list_projects`), через schema migration | Retrieval не меняет; полезно для диагностики, но требует чтения refs (per-worktree HEAD) — не сейчас |
| Q6 | Git-крейты | **Не добавлять**; gix (объём дерева), git2 (C-сборка), shell-out `git` (нет гарантии наличия) отклонены | Constitution: крейты только из design §7; нулевые внешние процессовые зависимости |

## 5. Краевые случаи (чеклист)

- **Detached HEAD / unborn branch** (`git init` без коммитов): `.git` есть → root = каталог;
  индексация работает (файлы на диске).
- **Bare repo**: `.git` нет → `git_root` вернёт саму директорию; `.md` там обычно нет — ок.
- **Submodule**: `.git`-файл → root = каталог submodule; чужая common dir не читается.
- **Worktree удалён** (`git worktree remove`): проект остаётся в реестре, root исчезает —
  `sync`/`status` должны давать понятную ошибку (проверить; кандидат теста).
- **Nested repo** внутри проекта: `.git` скрыт, но вложенные `.md` индексируются как обычные —
  ожидаемое поведение для docs-памяти (игноры вложенного репо не применяются, т.к. nearest
  git root выбран выше него).
- **`.git`-файл «garbage»/symlink**: ignore crate сквошит ошибку; наш `git_root` содержимое
  не читает — безопасно.
- **Sparse checkout**: отсутствующие файлы просто не находятся — ок.
- **CRLF/спецсимволы в путях**: актуально только если когда-то будем парсить porcelain —
  использовать `-z` и `core.quotePath=false`.

## 6. Security-связка (`docs/specs/security-review.md`)

- Containment строится на `project.canonical_root` (worktree) и **не должен** использовать
  `git_common_root`: иначе сессия в linked worktree получила бы доступ к docs main-worktree,
  то есть кросс-проектную утечку (нарушение §3.4).
- T47 (policy `index_project`) дополнительно закрывает регистрацию чужих worktree/репо
  произвольным абсолютным путём из MCP.
- «`.git`-файл» — недоверенный вход: если когда-нибудь будем читать gitdir/commondir,
  путь нужно канонизировать и валидировать (symlink/абсолютность), не выходя за
  `canonical_root`.

## 7. Кандидаты задач (не созданы)

- **G1 (тесты, малая)**: worktree-регресс в `tests/walk.rs`/`tests/registry.rs` — `.git`-файл +
  `commondir` + `info/exclude` соблюдается; subdir → git root; main+wt = два проекта.
- **G2 (фаза 2, опционально)**: `branch`/`head_sha` в `status`/`list_projects` (schema
  migration; чтение per-worktree HEAD).
- **G3 (по необходимости)**: диагностика `status` — показывать git root/common dir.
- **G4 (по необходимости)**: удалённый worktree — понятная ошибка `sync`/`status` (если G1 не
  покроет).
- Кандидат в watcher-оптимизацию (HEAD-watch для очень больших репо) сознательно не включён:
  сначала замер на реальных репо.
