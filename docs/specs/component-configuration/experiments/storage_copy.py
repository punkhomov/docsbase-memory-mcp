"""Probe metadata copy cost using the repository's original SQLite tables."""

from pathlib import Path
from tempfile import TemporaryDirectory
import json
import re
import shutil
import sqlite3
import time

repo = Path(__file__).resolve().parents[4]
source = (repo / "src/store/migrations.rs").read_text()
schema = re.search(r'const SCHEMA_V1: &str = "(.*?)";', source, re.S)[1]
target = 20_000
chunks_per_doc = 4

def size(path):
    return path.stat().st_size if path.exists() else 0

def configure(connection, mode):
    connection.execute(f"PRAGMA journal_mode = {mode}")
    connection.execute("PRAGMA synchronous = FULL")
    connection.execute("PRAGMA cache_size = -8192")
    connection.execute("PRAGMA foreign_keys = ON")
    if mode == "WAL":
        connection.execute("PRAGMA wal_autocheckpoint = 0")

def copy_rows_variant(db):
    tree = db.execute("PRAGMA page_count").fetchone()[0]
    page_bytes = db.execute("PRAGMA page_size").fetchone()[0]
    old = sqlite3.connect(db.execute("PRAGMA database_list").fetchone()[2])
    # Pin an old snapshot to exercise worst-case retention while writing.
    old.execute("BEGIN")
    before = old.execute("SELECT count(*) FROM chunks").fetchone()[0]
    start = time.perf_counter()
    with db:
        db.execute("INSERT INTO projects VALUES(2, '/synthetic/new', 'new', 'indexed', 3, 0, 0)")
        db.execute("""
            INSERT INTO docs
            SELECT id + ?, 2, rel_path, abs_path, title, frontmatter_json, size, mtime,
                   content_hash, indexed_at
            FROM docs WHERE project_id = 1
        """, (target,))
        db.execute("""
            INSERT INTO chunks
            SELECT id + ?, doc_id + ?, seq, heading_path, kind, lang,
                   line_start, line_end, text
            FROM chunks WHERE id <= ?
        """, (target * chunks_per_doc, target, target * chunks_per_doc))
        db.execute("UPDATE chunks SET text = text || ' changed' WHERE id > ? AND id % 200 = 0",
                   (target * chunks_per_doc,))
    duration = time.perf_counter() - start
    assert old.execute("SELECT count(*) FROM chunks").fetchone()[0] == before
    db_path = Path(db.execute("PRAGMA database_list").fetchone()[2])
    wal = size(db_path.with_name(db_path.name + "-wal"))
    on_disk = size(db_path) + wal
    old.rollback()
    old.close()
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    assert db.execute("SELECT count(*) FROM chunks").fetchone()[0] == 2 * before
    return dict(seconds=round(duration, 3), base_bytes=tree * page_bytes,
                peak_db_and_wal_bytes=on_disk, peak_wal_bytes=wal, final_bytes=size(db_path))

def backup_variant(base, stage):
    old = sqlite3.connect(base)
    new = sqlite3.connect(stage)
    configure(new, "DELETE")
    started = time.perf_counter()
    old.backup(new, pages=256)
    configure(new, "DELETE")
    with new:
        new.execute("UPDATE chunks SET text = text || ' changed' WHERE id % 200 = 0")
        journal_bytes = size(stage.with_name(stage.name + "-journal"))
        observed_bytes = size(base) + size(stage) + journal_bytes
    duration = time.perf_counter() - started
    assert new.execute("SELECT count(*) FROM chunks").fetchone()[0] == target * chunks_per_doc
    assert old.execute("SELECT count(*) FROM chunks WHERE text LIKE '%changed'").fetchone()[0] == 0
    new.close()
    old.close()
    return dict(seconds=round(duration, 3), base_bytes=size(base),
                staged_bytes=size(stage), journal_bytes=journal_bytes,
                observed_db_and_journal_bytes=observed_bytes,
                final_two_files_bytes=size(base) + size(stage))

with TemporaryDirectory(prefix="docsbase-cf10-probe-") as tmp:
    base = Path(tmp) / "base.sqlite"
    stage = Path(tmp) / "generation.sqlite"
    rows = Path(tmp) / "shared.sqlite"
    seed = sqlite3.connect(base)
    configure(seed, "WAL")
    seed.executescript(schema)
    with seed:
        seed.execute("INSERT INTO projects VALUES(1, '/synthetic/old', 'old', 'indexed', 3, 0, 0)")
        seed.executemany("INSERT INTO docs VALUES (?, 1, ?, ?, ?, NULL, 2400, 0, ?, 0)", (
            (i, f"guide/{i}.md", f"/synthetic/old/guide/{i}.md", f"Page {i}", f"hash-{i}")
            for i in range(1, target + 1)))
        seed.executemany("INSERT INTO chunks VALUES (?, ?, ?, 'Guide', 'prose', NULL, 1, 8, ?)", (
            ((i - 1) * chunks_per_doc + j, i, j, (f"Document {i} paragraph {j} includes a bounded metadata fixture. " * 12)[:600])
            for i in range(1, target + 1) for j in range(1, chunks_per_doc + 1)))
    seed.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    seed.close()
    stage_result = backup_variant(base, stage)
    shutil.copyfile(base, rows)
    db = sqlite3.connect(rows)
    configure(db, "WAL")
    row_result = copy_rows_variant(db)
    db.close()
    print(json.dumps({"sqlite": sqlite3.sqlite_version, "docs": target, "chunks": target * chunks_per_doc,
                      "shared_row_copy": row_result, "per_generation_backup": stage_result}, indent=2))
