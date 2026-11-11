# План T20 — 2026-11-11: подготовка эксперимента #71

Исполнение: superpowers:executing-plans, один task с fresh review.
Spec: REP-71-1…4 в requirements.md и dated design delta.

**Цель:** воспроизводимо выгрузить фактически индексируемые inputs и identity
для будущего сравнения representations; не менять production search.

Варианты: немедленная смена provider inputs требует ещё не полученных
recordings/cache decisions; отдельный поисковый runner дублирует pipeline.
Выбран маленький offline materializer поверх существующего build_env.

**Ограничения:** Rust1.88, cli-full; existing crates only; src/daemon/config,
fixtures/baseline/CI не изменяются; provider calls отсутствуют. Все исходные
citations/qrels сохраняются. Quality/tokens/live costs не выводятся из bytes.

**Файлы/интерфейсы:** tests/search_representation.rs — real-index regression
и ручной ignored entrypoint; tests/support/representation.rs — prepare(env,
cases, expected_corpus_hash, revision)->Result<Value>, write_new(path,value).
Output — advisory JSON с точными strings/hashes, четырьмя recipe pairs,
original cases, source/model/tokenizer pins и counts. Входные query strings
не обогащаются. Query keys отделены от document-only recipe changes.

- [x] RED: новый indexed duplicate-body/разные parent headings test падает
  с отсутствующим preflight. Никакого mock desired search behavior.
- [x] GREEN: SQL join по path/seq, raw/context strings и identities,
  оригинальные spans; blob-set и chunk membership различаются.
- [x] Проверить independent-index determinism, relocation/raw reuse,
  wrong corpus rejection, no-clobber и output вне checkout.
- [x] Focused tests, frozen gate, fmt/clippy, fresh task review, ledger/commit.
- [x] Manual ignored export на clean commit вне checkout; проверить полный
  frozen corpus/cases/bytes, source identity и отсутствие fixture mutations.
- [ ] Fresh final review и required native CI, отдельный PR; #71 остаётся
  открытой до измеренных results. Inference scope не расширяется.

Команда после task commit (output parent должен существовать, файл — нет):

```sh
DOCSBASE_REPRESENTATION_OUT=/tmp/representation-preflight.json \
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features cli-full \
  --test search_representation -- --ignored --exact \
  prepare_frozen_representation_experiment --nocapture
```

Artifact — проверяемая подготовка, не quality report или live runner.
Будущий collector использует те же recipes и точные упорядоченные candidate
pools штатного search; отдельный live scope задаёт модели, requests/retries,
usage/cost caps и доступность route. Сохранённые старые scores не подходят
для изменённых inputs; raw controls нужны для проверки provider drift.
