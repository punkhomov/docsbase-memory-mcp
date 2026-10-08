//! Golden metrics for the `search-quality` spec (S1; FR-1, FR-2, SC-1).
//!
//! The corpus lives in `tests/fixtures/search-quality/`; the baseline is a
//! checked-in report with tolerances (`baseline.json`), not a float snapshot:
//! recall/hit flags are discrete, nDCG compares within a small tolerance
//! (design §4.5).
//!
//! Regenerate the baseline deliberately:
//! `cargo test --locked --test search_quality -- --ignored regenerate_baseline`

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::tools;
use docsbase_memory::index::job::run_full;
use docsbase_memory::index::tantivy_index::IndexHandle;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::{DB_FILE, Db};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-quality")
}

fn baseline_path() -> PathBuf {
    fixture_root().join("baseline.json")
}

/// One golden query. `hit` means: at least one `expected` path is in top-`k`
/// and none of the `absent` paths is. `absent` encodes negative cases such as
/// `"plan id"` must not match `assessment_plan_id` (FR-2).
#[derive(Clone, Copy)]
struct Case {
    query: &'static str,
    class: &'static str,
    expected: &'static [&'static str],
    absent: &'static [&'static str],
    k: usize,
}

const CASES: &[Case] = &[
    // RU morphology: word forms must reach the same document (FR-9; SQ7).
    Case {
        query: "замена",
        class: "ru",
        expected: &["ru/binary-replace.md"],
        absent: &[],
        k: 3,
    },
    Case {
        query: "замены",
        class: "ru",
        expected: &["ru/binary-replace.md"],
        absent: &[],
        k: 3,
    },
    Case {
        query: "заменой",
        class: "ru",
        expected: &["ru/binary-replace.md"],
        absent: &[],
        k: 3,
    },
    Case {
        query: "каталоги",
        class: "ru",
        expected: &["ru/system-dirs.md"],
        absent: &[],
        k: 3,
    },
    Case {
        query: "каталогов",
        class: "ru",
        expected: &["ru/system-dirs.md"],
        absent: &[],
        k: 3,
    },
    // EN baseline (no regression source).
    Case {
        query: "atomic replace",
        class: "en",
        expected: &["en/atomic-replace.md"],
        absent: &[],
        k: 3,
    },
    Case {
        query: "system directories",
        class: "en",
        expected: &["en/system-dirs.md"],
        absent: &[],
        k: 3,
    },
    // Exact identifiers (FR-10 protection; SQ7 must not break these).
    Case {
        query: "assessment_plan_id",
        class: "identifiers",
        expected: &["api/assessment-plan.md"],
        absent: &[],
        k: 3,
    },
    Case {
        query: "MAX_FRAME_BYTES",
        class: "identifiers",
        expected: &["api/frame-limits.md"],
        absent: &[],
        k: 3,
    },
    // Phrase case: `"plan id"` must find the prose doc and must not match the
    // identifier document (FR-7; SQ6).
    Case {
        query: "\"plan id\"",
        class: "phrases",
        expected: &["plans/plan-lifecycle.md"],
        absent: &["api/assessment-plan.md"],
        k: 3,
    },
    // CJK (FR-12; SQ11).
    Case {
        query: "東京",
        class: "cjk",
        expected: &["cjk/airport.md"],
        absent: &[],
        k: 3,
    },
    // Arabic without harakat must find the vocalized document (FR-13; SQ10).
    Case {
        query: "مطار",
        class: "arabic",
        expected: &["ar/airport.md"],
        absent: &[],
        k: 3,
    },
];

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct CaseReport {
    query: String,
    class: String,
    k: usize,
    hit: bool,
    /// Whether a document from `absent` reached top-k (negative half of SC-3:
    /// `"plan id"` must not match `assessment_plan_id`).
    absent_hit: bool,
    ndcg: f64,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct ClassReport {
    hit_rate: f64,
    ndcg: f64,
}

#[derive(Serialize, Deserialize, Debug)]
struct Tolerance {
    ndcg: f64,
}

#[derive(Serialize, Deserialize, Debug)]
struct Baseline {
    tolerance: Tolerance,
    cases: Vec<CaseReport>,
    classes: BTreeMap<String, ClassReport>,
}

const NDCG_TOLERANCE: f64 = 0.02;

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for entry in fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture");
        }
    }
}

struct Bench {
    cache: TempDir,
    _root: TempDir,
    db: Db,
    _index: IndexHandle,
    project: Project,
}

impl Bench {
    fn new() -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("root");
        copy_dir(&fixture_root(), root.path());
        let mut db = Db::open(cache.path()).expect("db");
        let canonical_root = root.path().canonicalize().expect("canonical root");
        let conn = Connection::open(cache.path().join(DB_FILE)).expect("raw db");
        conn.execute(
            "INSERT INTO projects (id, canonical_root, name, status, schema_version, created_at)
             VALUES (1, ?1, 'search-quality', 'not_indexed', 1, 0)",
            [canonical_root.to_string_lossy().as_ref()],
        )
        .expect("insert project");
        drop(conn);
        let mut index =
            IndexHandle::open_or_create(&cache.path().join("projects/1/tantivy")).expect("index");
        let mut project = Project {
            id: 1,
            canonical_root,
            name: "search-quality".to_owned(),
            status: ProjectStatus::NotIndexed,
            schema_version: docsbase_memory::store::migrations::SCHEMA_VERSION,
            created_at: 0,
            last_indexed_at: None,
        };
        let stats = run_full(&mut db, &mut index, &project, &Config::default()).expect("run_full");
        assert!(stats.docs >= 10, "corpus indexed: {stats:?}");
        project.status = ProjectStatus::Indexed;
        Self {
            cache,
            _root: root,
            db,
            _index: index,
            project,
        }
    }

    fn paths(&self, query: &str, limit: usize) -> Vec<String> {
        let value = tools::search_docs(
            &self.db,
            self.cache.path(),
            &self.project,
            &json!({ "query": query, "limit": limit }),
        )
        .expect("search_docs");
        value
            .as_array()
            .expect("citation array")
            .iter()
            .map(|row| row["path"].as_str().expect("path").to_owned())
            .collect()
    }
}

/// Discounted gain of the first relevant document within the fetched top-`k`
/// (IDCG = 1 for a single expected document); 0.0 when it is missing.
fn ndcg_at_k(paths: &[String], expected: &[&str]) -> f64 {
    for (rank, path) in paths.iter().enumerate() {
        if expected.contains(&path.as_str()) {
            return 1.0 / (count(rank) + 2.0).log2();
        }
    }
    0.0
}

/// Lossless count → f64 (test counters fit u32 by construction).
fn count(value: usize) -> f64 {
    f64::from(u32::try_from(value).expect("count fits u32"))
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn evaluate(bench: &Bench) -> Baseline {
    let mut cases = Vec::with_capacity(CASES.len());
    let mut class_hits: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut class_ndcg: BTreeMap<&str, (f64, usize)> = BTreeMap::new();

    for case in CASES {
        let paths = bench.paths(case.query, case.k);
        let found = case
            .expected
            .iter()
            .any(|p| paths.contains(&(*p).to_owned()));
        let absent_found = case.absent.iter().any(|p| paths.contains(&(*p).to_owned()));
        let hit = found && !absent_found;
        let ndcg = if absent_found {
            0.0
        } else {
            ndcg_at_k(&paths, case.expected)
        };
        let entry = class_hits.entry(case.class).or_insert((0, 0));
        entry.0 += usize::from(hit);
        entry.1 += 1;
        let ndcg_entry = class_ndcg.entry(case.class).or_insert((0.0, 0));
        ndcg_entry.0 += ndcg;
        ndcg_entry.1 += 1;
        cases.push(CaseReport {
            query: case.query.to_owned(),
            class: case.class.to_owned(),
            k: case.k,
            hit,
            absent_hit: absent_found,
            ndcg: round4(ndcg),
        });
    }

    let classes = class_hits
        .iter()
        .map(|(class, (hits, total))| {
            let (ndcg_sum, ndcg_total) = class_ndcg.get(class).copied().unwrap_or((0.0, 1));
            let report = ClassReport {
                hit_rate: round4(count(*hits) / count(*total)),
                ndcg: round4(ndcg_sum / count(ndcg_total)),
            };
            ((*class).to_owned(), report)
        })
        .collect();

    Baseline {
        tolerance: Tolerance {
            ndcg: NDCG_TOLERANCE,
        },
        cases,
        classes,
    }
}

fn load_baseline() -> Baseline {
    let bytes = fs::read(baseline_path()).expect(
        "tests/fixtures/search-quality/baseline.json is missing — \
         run: cargo test --locked --test search_quality -- --ignored regenerate_baseline",
    );
    serde_json::from_slice(&bytes).expect("parse baseline.json")
}

/// Pure comparison behind [`no_regression_gate`]: regressions only, no
/// equality requirement (strict reproducibility lives in
/// [`metrics_match_baseline`]).
fn regression_failures(current: &Baseline, baseline: &Baseline) -> Vec<String> {
    let mut failures = Vec::new();

    for (class, expected) in &baseline.classes {
        let Some(report) = current.classes.get(class) else {
            failures.push(format!("[{class}] class missing in the current run"));
            continue;
        };
        if report.hit_rate + 1e-9 < expected.hit_rate {
            failures.push(format!(
                "[{class}] hit_rate {:.4} fell below baseline {:.4}",
                report.hit_rate, expected.hit_rate
            ));
        }
        if report.ndcg + baseline.tolerance.ndcg < expected.ndcg {
            failures.push(format!(
                "[{class}] ndcg {:.4} fell below baseline {:.4} (tol {:.2})",
                report.ndcg, expected.ndcg, baseline.tolerance.ndcg
            ));
        }
    }

    let current_by_key: BTreeMap<(&str, &str), &CaseReport> = current
        .cases
        .iter()
        .map(|case| ((case.query.as_str(), case.class.as_str()), case))
        .collect();
    for base_case in &baseline.cases {
        let Some(cur) = current_by_key.get(&(base_case.query.as_str(), base_case.class.as_str()))
        else {
            failures.push(format!(
                "[{}] {:?}: case missing in the current run",
                base_case.class, base_case.query
            ));
            continue;
        };
        if base_case.hit && !cur.hit {
            failures.push(format!(
                "[{}] {:?}: relevant document dropped from top-{}",
                base_case.class, base_case.query, base_case.k
            ));
        }
        if !base_case.absent_hit && cur.absent_hit {
            failures.push(format!(
                "[{}] {:?}: forbidden document appeared in top-{}",
                base_case.class, base_case.query, base_case.k
            ));
        }
        if cur.ndcg + baseline.tolerance.ndcg < base_case.ndcg {
            failures.push(format!(
                "[{}] {:?}: ndcg {:.4} fell below baseline {:.4}",
                base_case.class, base_case.query, cur.ndcg, base_case.ndcg
            ));
        }
    }

    failures
}

/// SC-1: metrics are reproducible — current run matches the checked-in
/// baseline within tolerance.
#[test]
fn metrics_match_baseline() {
    let bench = Bench::new();
    let current = evaluate(&bench);
    let baseline = load_baseline();

    assert_eq!(
        current.cases.len(),
        baseline.cases.len(),
        "baseline case count drift (regenerate deliberately if intended)"
    );
    assert_eq!(
        current.classes.keys().collect::<Vec<_>>(),
        baseline.classes.keys().collect::<Vec<_>>(),
        "baseline class set drift (regenerate deliberately if intended)"
    );

    let mut failures = Vec::new();
    for (current_case, baseline_case) in current.cases.iter().zip(baseline.cases.iter()) {
        if current_case.query != baseline_case.query || current_case.class != baseline_case.class {
            failures.push(format!(
                "case order drift: {} vs {}",
                current_case.query, baseline_case.query
            ));
            continue;
        }
        if current_case.k != baseline_case.k {
            failures.push(format!(
                "[{}] {:?}: k {} != baseline {}",
                current_case.class, current_case.query, current_case.k, baseline_case.k
            ));
        }
        if current_case.hit != baseline_case.hit {
            failures.push(format!(
                "[{}] {:?}: hit {} != baseline {}",
                current_case.class, current_case.query, current_case.hit, baseline_case.hit
            ));
        }
        if current_case.absent_hit != baseline_case.absent_hit {
            failures.push(format!(
                "[{}] {:?}: absent_hit {} != baseline {}",
                current_case.class,
                current_case.query,
                current_case.absent_hit,
                baseline_case.absent_hit
            ));
        }
        if (current_case.ndcg - baseline_case.ndcg).abs() > baseline.tolerance.ndcg {
            failures.push(format!(
                "[{}] {:?}: ndcg {:.4} != baseline {:.4} (tol {:.2})",
                current_case.class,
                current_case.query,
                current_case.ndcg,
                baseline_case.ndcg,
                baseline.tolerance.ndcg
            ));
        }
    }

    for (class, report) in &current.classes {
        let expected = baseline
            .classes
            .get(class)
            .unwrap_or_else(|| panic!("class {class} missing from baseline"));
        if (report.hit_rate - expected.hit_rate).abs() > 1e-9 {
            failures.push(format!(
                "[{class}] hit_rate {:.4} != baseline {:.4}",
                report.hit_rate, expected.hit_rate
            ));
        }
        if (report.ndcg - expected.ndcg).abs() > baseline.tolerance.ndcg {
            failures.push(format!(
                "[{class}] ndcg {:.4} != baseline {:.4}",
                report.ndcg, expected.ndcg
            ));
        }
    }

    for (class, report) in &current.classes {
        println!(
            "class {class}: hit_rate={:.2} ndcg={:.4}",
            report.hit_rate, report.ndcg
        );
    }

    assert!(
        failures.is_empty(),
        "baseline drift:\n{}",
        failures.join("\n")
    );
}

/// FR-3/SC-4: acceptance gate — no relevant document may drop out of top-k and
/// no class metric may fall below the checked-in baseline (nDCG within
/// tolerance). Improvements are allowed; citation snapshots update only as
/// deliberate deltas (S2), ranks are guarded by hit@k here.
/// FR-11 guard: the tuned boost/penalty values must keep every class metric
/// at or above the checked-in baseline (SQ8 step 3; SQ2's gate reused).
#[test]
fn no_regression_gate() {
    let bench = Bench::new();
    let current = evaluate(&bench);
    let baseline = load_baseline();
    let failures = regression_failures(&current, &baseline);
    assert!(
        failures.is_empty(),
        "search-quality regressions:\n{}",
        failures.join("\n")
    );
}

/// The gate itself rejects synthetic regressions — a lost hit and a newly
/// appearing forbidden document (negative half of SC-3) — and accepts a
/// synthetic improvement.
#[test]
fn gate_rejects_regression() {
    let make = |hit: bool, absent_hit: bool, ndcg: f64, class_ndcg: f64| Baseline {
        tolerance: Tolerance {
            ndcg: NDCG_TOLERANCE,
        },
        cases: vec![CaseReport {
            query: "замены".to_owned(),
            class: "ru".to_owned(),
            k: 3,
            hit,
            absent_hit,
            ndcg,
        }],
        classes: BTreeMap::from([(
            "ru".to_owned(),
            ClassReport {
                hit_rate: f64::from(u8::from(hit)),
                ndcg: class_ndcg,
            },
        )]),
    };

    // Baseline keeps the hit but with zero nDCG so only the drop branch fires.
    let baseline = make(true, false, 0.0, 0.0);
    let lost_hit = make(false, false, 0.0, 0.0);
    let failures = regression_failures(&lost_hit, &baseline);
    assert!(
        failures
            .iter()
            .any(|line| line.contains("dropped from top-")),
        "lost hit must be reported: {failures:?}"
    );

    // Negative half: a forbidden document appears where the baseline had none.
    let clean = make(false, false, 0.0, 0.0);
    let forbidden = make(false, true, 0.0, 0.0);
    let failures = regression_failures(&forbidden, &clean);
    assert!(
        failures
            .iter()
            .any(|line| line.contains("forbidden document appeared")),
        "appearing forbidden document must be reported: {failures:?}"
    );

    let improved_current = make(true, false, 1.0, 1.0);
    assert!(
        regression_failures(&improved_current, &clean).is_empty(),
        "improvements must pass the gate"
    );
}

/// Deliberate baseline regeneration (never part of the normal suite).
#[test]
#[ignore = "regenerates tests/fixtures/search-quality/baseline.json"]
fn regenerate_baseline() {
    let bench = Bench::new();
    let baseline = evaluate(&bench);
    let bytes = serde_json::to_vec_pretty(&baseline).expect("serialize baseline");
    fs::write(baseline_path(), bytes).expect("write baseline.json");
    println!("baseline written to {}", baseline_path().display());
}
