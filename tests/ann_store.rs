//! `AnnStore` persistence contract (spec ann-index T2): paths, key, metadata,
//! atomic write/load, stash/corrupt rejection and generation cleanup.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use docsbase_memory::hybrid::ann_store::{ANN_FORMAT_VERSION, AnnKey, AnnStore, chunk_set_digest};

fn key(
    project: i64,
    generation: i64,
    model: &str,
    dim: usize,
    count: usize,
    digest: &str,
) -> AnnKey {
    AnnKey {
        project_id: project,
        generation,
        model_id: model.to_owned(),
        dim,
        chunk_count: count,
        chunk_set_digest: digest.to_owned(),
        format_version: ANN_FORMAT_VERSION,
    }
}

fn store() -> (TempDir, AnnStore) {
    let cache = TempDir::new().expect("cache");
    let store = AnnStore::new(cache.path().to_path_buf());
    (cache, store)
}

fn write_dump(dir: &Path) -> String {
    fs::write(dir.join("index.hnsw.graph"), b"graph bytes").expect("graph");
    fs::write(dir.join("index.hnsw.data"), b"data bytes").expect("data");
    "index".to_owned()
}

#[test]
fn load_returns_none_when_no_index_exists() {
    let (_cache, store) = store();
    let out = store
        .load(&key(1, 42, "model@d8", 8, 3, "abcdef01"))
        .expect("load");
    assert!(out.is_none());
}

#[test]
fn load_returns_none_without_ready_marker() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    let slot = store
        .persist_dump(&k, |dir| Ok(write_dump(dir)))
        .expect("persist");
    assert!(slot.dir.join("READY").is_file());
    fs::remove_file(slot.dir.join("READY")).expect("remove READY");
    let out = store.load(&k).expect("load");
    assert!(out.is_none());
}

#[test]
fn load_rejects_metadata_mismatch() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    store
        .persist_dump(&k, |dir| Ok(write_dump(dir)))
        .expect("persist");

    let other_gen = key(1, 43, "model@d8", 8, 3, "abcdef01");
    assert!(store.load(&other_gen).expect("load").is_none());

    let other_model = key(1, 42, "model@d16", 8, 3, "abcdef01");
    assert!(store.load(&other_model).expect("load").is_none());

    let other_digest = key(1, 42, "model@d8", 8, 3, "deadbeef");
    assert!(store.load(&other_digest).expect("load").is_none());

    let other_project = key(2, 42, "model@d8", 8, 3, "abcdef01");
    assert!(store.load(&other_project).expect("load").is_none());
}

#[test]
fn load_rejects_foreign_hnsw_version() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    let slot = store
        .persist_dump(&k, |dir| Ok(write_dump(dir)))
        .expect("persist");
    let meta_path = slot.dir.join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&meta_path).expect("meta")).expect("json");
    meta["hnsw_rs_version"] = serde_json::Value::String("0.3.4".to_owned());
    fs::write(&meta_path, serde_json::to_string(&meta).expect("serialize")).expect("write");
    assert!(store.load(&k).expect("load").is_none());
}

#[test]
fn load_rejects_corrupt_meta_json() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    let slot = store
        .persist_dump(&k, |dir| Ok(write_dump(dir)))
        .expect("persist");
    fs::write(slot.dir.join("meta.json"), "{ not json").expect("write");
    assert!(store.load(&k).expect("load").is_none());
}

#[test]
fn load_rejects_oversized_dump_files() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    let slot = store
        .persist_dump(&k, |dir| Ok(write_dump(dir)))
        .expect("persist");
    let data = slot.dir.join("index.hnsw.data");
    fs::OpenOptions::new()
        .write(true)
        .open(&data)
        .expect("open data")
        .set_len(docsbase_memory::hybrid::ann_store::MAX_ANN_FILE_BYTES + 1)
        .expect("grow data");
    assert!(store.load(&k).expect("load").is_none());
}

#[test]
fn persist_dump_publishes_and_is_loadable() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    let slot = store
        .persist_dump(&k, |dir| Ok(write_dump(dir)))
        .expect("persist");
    assert_eq!(slot.meta.project_id, 1);
    assert_eq!(slot.meta.generation, 42);
    assert_eq!(slot.meta.model_id, "model@d8");
    assert_eq!(slot.meta.dim, 8);
    assert_eq!(slot.meta.count, 3);
    assert_eq!(slot.meta.chunk_set_digest, "abcdef01");
    assert_eq!(slot.meta.format, ANN_FORMAT_VERSION);
    for name in ["meta.json", "index.hnsw.graph", "index.hnsw.data", "READY"] {
        assert!(slot.dir.join(name).is_file(), "missing {name}");
    }
    let loaded = store.load(&k).expect("load").expect("slot");
    assert_eq!(loaded.meta, slot.meta);
    assert_eq!(loaded.dir, slot.dir);
    assert!(slot.dir.to_string_lossy().contains("/projects/1/ann/"));
}

#[test]
fn persist_dump_is_not_visible_before_rename() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    let visible = std::cell::Cell::new(None);
    let slot = store
        .persist_dump(&k, |dir| {
            let pending = store.load(&k).expect("load during write");
            visible.set(Some(pending.is_none()));
            Ok(write_dump(dir))
        })
        .expect("persist");
    assert!(visible.get().unwrap_or(false), "partial dir was visible");
    assert!(store.load(&k).expect("load").is_some());
    assert!(
        !slot
            .dir
            .file_name()
            .expect("dir name")
            .to_string_lossy()
            .starts_with("tmp-")
    );
}

#[test]
fn persist_dump_rejects_non_index_basename() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    let err = store
        .persist_dump(&k, |dir| {
            fs::write(dir.join("other.hnsw.graph"), b"graph").expect("graph");
            Ok("other".to_owned())
        })
        .expect_err("basename");
    assert!(err.to_string().contains("basename"), "{err}");
    assert!(store.load(&k).expect("load").is_none());
}

#[test]
fn invalidate_removes_the_project_ann_tree() {
    let (_cache, store) = store();
    for generation in 1..=3 {
        let k = key(1, generation, "model@d8", 8, 3, "abcdef01");
        store
            .persist_dump(&k, |dir| Ok(write_dump(dir)))
            .expect("persist");
    }
    store.invalidate(1).expect("invalidate");
    assert!(!store.project_ann_root(1).exists());
    assert!(
        store
            .load(&key(1, 2, "model@d8", 8, 3, "abcdef01"))
            .expect("load")
            .is_none()
    );
}

#[test]
fn cleanup_keeps_last_two_generations() {
    let (_cache, store) = store();
    let gen1 = key(1, 1, "model@d8", 8, 3, "abcdef01");
    let gen2 = key(1, 2, "model@d8", 8, 3, "abcdef01");
    let gen3 = key(1, 3, "model@d8", 8, 3, "abcdef01");
    for k in [&gen1, &gen2, &gen3] {
        store
            .persist_dump(k, |dir| Ok(write_dump(dir)))
            .expect("persist");
    }
    let tmp = store.project_ann_root(1).join("tmp-0-456");
    fs::create_dir_all(&tmp).expect("tmp dir");
    fs::write(tmp.join("index.hnsw.graph"), b"graph").expect("write");

    store.cleanup(1).expect("cleanup");

    assert!(store.load(&gen1).expect("load").is_none(), "gen 1 removed");
    assert!(store.load(&gen2).expect("load").is_some());
    assert!(store.load(&gen3).expect("load").is_some());
    assert!(!tmp.exists(), "tmp dir removed");
    let mut left: Vec<String> = fs::read_dir(store.project_ann_root(1))
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    left.sort();
    assert_eq!(left.len(), 2, "{left:?}");
}

#[test]
fn cleanup_skips_the_current_process_tmp() {
    let (_cache, store) = store();
    let tmp = store
        .project_ann_root(1)
        .join(format!("tmp-{}-999", std::process::id()));
    fs::create_dir_all(&tmp).expect("tmp dir");
    fs::write(tmp.join("index.hnsw.graph"), b"graph").expect("write");

    store.cleanup(1).expect("cleanup");
    assert!(
        tmp.exists(),
        "an in-flight build's tmp must survive cleanup"
    );
}

#[test]
fn cleanup_keeps_two_generations_per_model() {
    let (_cache, store) = store();
    for model in ["model-a@d8", "model-b@d8"] {
        for generation in 1..=3 {
            let k = key(1, generation, model, 8, 3, "abcdef01");
            store
                .persist_dump(&k, |dir| Ok(write_dump(dir)))
                .expect("persist");
        }
    }

    store.cleanup(1).expect("cleanup");

    for model in ["model-a@d8", "model-b@d8"] {
        assert!(
            store
                .load(&key(1, 1, model, 8, 3, "abcdef01"))
                .expect("load")
                .is_none(),
            "{model}: oldest generation removed"
        );
        assert!(
            store
                .load(&key(1, 2, model, 8, 3, "abcdef01"))
                .expect("load")
                .is_some(),
            "{model}: gen 2 kept"
        );
        assert!(
            store
                .load(&key(1, 3, model, 8, 3, "abcdef01"))
                .expect("load")
                .is_some(),
            "{model}: gen 3 kept"
        );
    }
}

#[test]
fn chunk_set_digest_is_order_independent_and_content_sensitive() {
    let chunks = vec![
        (7, "c".to_owned()),
        (1, "a".to_owned()),
        (5, "b".to_owned()),
    ];
    let shuffled = vec![
        (5, "b".to_owned()),
        (1, "a".to_owned()),
        (7, "c".to_owned()),
    ];
    let a = chunk_set_digest(&chunks);
    let b = chunk_set_digest(&shuffled);
    assert_eq!(a, b);
    assert_ne!(
        a,
        chunk_set_digest(&[(1, "a".to_owned()), (5, "b".to_owned())])
    );
    assert_eq!(a.len(), 64);
}

#[test]
fn cleanup_disk_keeps_the_newest_index_under_budget() {
    let (_cache, store) = store();
    let mut keys = Vec::new();
    for project in 1..=3 {
        let k = key(project, 1, "model@d8", 8, 3, "abcdef01");
        store
            .persist_dump(&k, |dir| Ok(write_dump(dir)))
            .expect("persist");
        std::thread::sleep(std::time::Duration::from_millis(20));
        keys.push(k);
    }

    store.cleanup_disk(0, None).expect("cleanup disk");

    assert!(
        store.load(&keys[2]).expect("load newest").is_some(),
        "the newest index must survive"
    );
    assert!(store.load(&keys[0]).expect("load first").is_none());
    assert!(store.load(&keys[1]).expect("load second").is_none());
}

#[test]
fn failed_publish_removes_the_temporary_directory() {
    let (_cache, store) = store();
    let k = key(1, 42, "model@d8", 8, 3, "abcdef01");
    let final_dir = store.ann_dir(&k);
    fs::create_dir_all(final_dir.parent().expect("parent")).expect("parent dir");
    fs::write(&final_dir, b"not a directory").expect("block final dir");

    let err = store
        .persist_dump(&k, |dir| Ok(write_dump(dir)))
        .expect_err("publish must fail");
    assert!(!err.to_string().is_empty());

    let leftovers: Vec<String> = fs::read_dir(store.project_ann_root(1))
        .expect("read ann root")
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "tmp leaked: {leftovers:?}");
}
