//! Full-index benchmark (NFR-1): 1000 markdown files through `run_full`.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use docsbase_memory::config::Config;
use docsbase_memory::index::job::run_full;
use docsbase_memory::index::tantivy_index::IndexHandle;
use docsbase_memory::store::migrations;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{DB_FILE, Db};
use tempfile::TempDir;

const FILES: usize = 1_000;

fn write_corpus(root: &Path) {
    for i in 0..FILES {
        let rel = format!("docs/section-{:02}/file-{i}.md", i % 20);
        let path = root.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        let mut body = format!("# Document {i}\n\nIntro prose about widget{i} and indexing.\n");
        for section in 0..8 {
            let _ = write!(
                body,
                "\n## Section {section}\n\nProse with latch{i}-{section} and \
                 performance notes for the search budget.\n\n```rust\nlet v = {i};\n```\n"
            );
        }
        fs::write(path, body).expect("write");
    }
}

struct Prepared {
    _cache: TempDir,
    _root: TempDir,
    db: Db,
    index: IndexHandle,
    project: Project,
}

fn prepare() -> Prepared {
    let cache = TempDir::new().expect("cache");
    let root = TempDir::new().expect("root");
    write_corpus(root.path());
    let db = Db::open(cache.path()).expect("db");
    let canonical_root = root.path().canonicalize().expect("canonical");
    let project = Project {
        id: 1,
        canonical_root,
        name: "bench".to_owned(),
        status: ProjectStatus::NotIndexed,
        schema_version: migrations::SCHEMA_VERSION,
        created_at: 0,
        last_indexed_at: None,
    };
    let mut index =
        IndexHandle::open_or_create(&cache.path().join("projects/1/tantivy")).expect("index");
    index.mark_rebuilt().expect("mark rebuilt");
    let conn = rusqlite::Connection::open(cache.path().join(DB_FILE)).expect("raw db");
    conn.execute(
        "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
         VALUES (1, ?1, 'bench', 'not_indexed', ?2, 0)",
        rusqlite::params![
            root.path()
                .canonicalize()
                .expect("canonical")
                .to_string_lossy(),
            migrations::SCHEMA_VERSION,
        ],
    )
    .expect("insert project");
    drop(conn);
    Prepared {
        _cache: cache,
        _root: root,
        db,
        index,
        project,
    }
}

fn full_index(c: &mut Criterion) {
    c.bench_function("index_1000_files", |b| {
        b.iter_batched(
            prepare,
            |mut prepared| {
                let stats = run_full(
                    &mut prepared.db,
                    &mut prepared.index,
                    &prepared.project,
                    &Config::default(),
                )
                .expect("run_full");
                assert_eq!(stats.docs, FILES);
                stats
            },
            BatchSize::PerIteration,
        );
    });
}

criterion_group!(benches, full_index);
criterion_main!(benches);
