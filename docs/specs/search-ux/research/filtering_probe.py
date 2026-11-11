"""Throwaway research: SQLite prefix semantics and RRF, not a docsbase benchmark."""
import json
import platform
import sqlite3
import statistics
import time


def successor(prefix):
    chars = list(prefix)
    while chars:
        value = ord(chars.pop())
        if value < 0x10FFFF:
            value += 1
            if 0xD800 <= value <= 0xDFFF:
                value = 0xE000
            return ''.join(chars) + chr(value)
    return None


conn = sqlite3.connect(':memory:')
conn.execute('CREATE TABLE docs(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, rel_path TEXT NOT NULL, UNIQUE(project_id,rel_path))')
conn.executemany('INSERT INTO docs VALUES(?,1,?)', [
    (i + 1, ('docs/' if i % 2 else 'other/') + f'{i:05}.md')
    for i in range(20000)
])
conn.commit()
sqls = {
    'range': ('SELECT id FROM docs WHERE project_id=? AND rel_path>=? AND rel_path<?', (1, 'docs/', 'docs0')),
    'substr': ('SELECT id FROM docs WHERE project_id=? AND substr(rel_path,1,length(?))=?', (1, 'docs/', 'docs/')),
    'like': ('SELECT id FROM docs WHERE project_id=? AND rel_path LIKE ?', (1, 'docs/%')),
}
out = {'python': platform.python_version(), 'sqlite': sqlite3.sqlite_version, 'fixture': '20k docs, interleaved ids, two equal folders; in-memory warm DB; execute+fetch ids, 11 samples', 'queries': {}}
oracle = set(r[0] for r in conn.execute(*sqls['range']))
assert len(oracle) == 10000
for name, (sql, args) in sqls.items():
    timings = []
    for _ in range(11):
        start = time.perf_counter_ns()
        rows = conn.execute(sql, args).fetchall()
        timings.append((time.perf_counter_ns() - start) / 1e6)
        assert set(r[0] for r in rows) == oracle
    out['queries'][name] = {'rows': len(rows), 'median_ms': round(statistics.median(timings), 4), 'plan': [r[3] for r in conn.execute('EXPLAIN QUERY PLAN ' + sql, args)]}

edge = ['', 'docs', 'docs/', 'docs-old/a', 'Docs/a', 'docs/%literal', 'docs/_literal', 'docs/é/a', 'docs/e\u0301/a', 'доки/a', '😀/a', chr(0x10FFFF), chr(0x10FFFF) + '/a', 'x' + chr(0x10FFFF), 'x' + chr(0x10FFFF) + '/a', chr(0xD7FF) + '/a', chr(0xE000) + '/a']
conn.executemany('INSERT INTO docs(project_id,rel_path) VALUES(2,?)', [(p,) for p in edge])
prefixes = ['', 'docs', 'docs/', 'Docs', 'docs/%', 'docs/_', 'docs/é', 'docs/e\u0301', 'доки/', '😀', chr(0x10FFFF), 'x' + chr(0x10FFFF), chr(0xD7FF)]
for prefix in prefixes:
    expected = {p for p in edge if p.startswith(prefix)}
    upper = successor(prefix)
    if not prefix:
        actual = {r[0] for r in conn.execute('SELECT rel_path FROM docs WHERE project_id=2')}
    elif upper is None:
        actual = {r[0] for r in conn.execute('SELECT rel_path FROM docs WHERE project_id=2 AND rel_path>=?', (prefix,))}
    else:
        actual = {r[0] for r in conn.execute('SELECT rel_path FROM docs WHERE project_id=2 AND rel_path>=? AND rel_path<?', (prefix, upper))}
    assert actual == expected, (prefix, actual, expected)
out['prefix_equivalence_cases'] = len(prefixes)
out['like_counterexample'] = {'prefix': 'docs/', 'like_matches_Docs_a': bool(conn.execute("SELECT 'Docs/a' LIKE 'docs/%'").fetchone()[0]), 'literal_startswith': 'Docs/a'.startswith('docs/')}

allowed = {'A', 'B', 'C'}
lexical = ['B'] + [f'denied_{i}' for i in range(98)] + ['A', 'C']
dense = ['A', 'C', 'B']
def rrf(lists):
    scores = {}
    for seq in lists:
        for rank, item in enumerate(seq, 1):
            scores[item] = scores.get(item, 0) + 1 / (60 + rank)
    return [(name, round(score, 9)) for name, score in sorted(scores.items(), key=lambda x: (-x[1], x[0])) if name in allowed]
out['rrf_global_then_filter'] = rrf([lexical, dense])
out['rrf_scoped_lists'] = rrf([[x for x in seq if x in allowed] for seq in [lexical, dense]])
assert out['rrf_global_then_filter'][0][0] == 'B'
assert out['rrf_scoped_lists'][0][0] == 'A'
out['memory_arithmetic_bytes'] = {'u64_10k_ids_payload_only': 10000 * 8, '20k_dense_ordinal_bitmap_only': (20000 + 7) // 8, 'bitmap_by_sparse_chunk_id_for_doc20k': (20000 << 32) // 8 + 1}
print(json.dumps(out, indent=2, ensure_ascii=False))
