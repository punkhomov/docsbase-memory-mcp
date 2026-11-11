//! Request-owned document scope. No cache and no retained SQL handles.

use std::mem::size_of;

use rusqlite::{Connection, params, params_from_iter};
use serde_json::Value;
use tantivy::Term;

use crate::error::{Error, Result};
use crate::index::tantivy_index::chunk_id;
use crate::store::repo::{BATCH_KEYS, db_error};
use crate::vector_cache::chunk_hash;

use super::budget::{PayloadBudget, PayloadPermit};
use super::operation::SearchOperation;

/// Data drops before the shared byte admission is returned.
pub(crate) struct DocumentScope {
    documents: Vec<i64>,
    _permit: PayloadPermit,
}

#[derive(Clone, Copy)]
pub(crate) struct ScopeRequest<'a> {
    pub project: i64,
    pub prefix: Option<&'a str>,
    pub tags: &'a [String],
    pub dense: bool,
}

impl DocumentScope {
    /// Caller owns a metadata snapshot and project read lease. The charge is
    /// a logical preparation allowance, not allocator/RSS instrumentation:
    /// IDs + term slots/encoded keys + maximum JSON parse slots + hash page.
    pub(crate) fn capture(
        connection: &Connection,
        request: ScopeRequest<'_>,
        budget: &PayloadBudget,
        operation: &SearchOperation,
    ) -> Result<Self> {
        operation.check()?;
        let ScopeRequest {
            project,
            prefix,
            tags,
            dense,
        } = request;
        let encoding: String = connection
            .pragma_query_value(None, "encoding", |row| row.get(0))
            .map_err(db_error)?;
        let utf8 = encoding == "UTF-8";
        let lower = prefix.unwrap_or("");
        let upper = if utf8 {
            prefix.and_then(prefix_end)
        } else {
            None
        };
        let range = if !utf8 {
            // UTF-16 BINARY ordering does not follow Unicode successors.
            // Preserve literal semantics with a project scan + Rust predicate.
            "project_id=?1 AND ?2 IS NOT NULL AND ?3 IS NULL"
        } else if upper.is_some() {
            "project_id=?1 AND rel_path>=?2 COLLATE BINARY AND rel_path<?3 COLLATE BINARY"
        } else {
            "project_id=?1 AND rel_path>=?2 COLLATE BINARY AND ?3 IS NULL"
        };
        let json_size = if tags.is_empty() {
            "0"
        } else {
            "COALESCE(MAX(octet_length(frontmatter_json)),0)"
        };
        let (count, json_bytes): (i64, i64) = connection
            .query_row(
                &format!("SELECT COUNT(*), {json_size} FROM docs WHERE {range}"),
                params![project, lower, upper],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(db_error)?;
        let count = u64::try_from(count).map_err(|_| overflow())?;
        let mut json_bytes = u64::try_from(json_bytes).map_err(|_| overflow())?;
        if encoding != "UTF-8" {
            // SQLite converts UTF-16 text to UTF-8 for rusqlite; one UTF-16
            // unit may become three UTF-8 bytes.
            json_bytes = json_bytes
                .checked_add(1)
                .and_then(|n| (n / 2).checked_mul(3))
                .ok_or_else(overflow)?;
        }
        // Every JSON byte is allowed one Value slot (conservative logical
        // allowance for container-heavy metadata); at most one row is parsed.
        let json_charge = if tags.is_empty() {
            0
        } else {
            json_bytes
                .checked_mul(size_of::<Value>() as u64)
                .ok_or_else(overflow)?
        };
        let per_doc = (size_of::<i64>() + size_of::<Term>() + size_of::<i64>()) as u64;
        let page_charge = if dense && count > 0 {
            (BATCH_KEYS * (size_of::<(u64, String)>() + 64)) as u64
        } else {
            0
        };
        let charge = count
            .checked_mul(per_doc)
            .and_then(|n| n.checked_add(json_charge))
            .and_then(|n| n.checked_add(page_charge))
            .ok_or_else(overflow)?;
        let permit = budget.reserve(charge)?;
        let capacity = usize::try_from(count).map_err(|_| overflow())?;
        let mut documents = Vec::with_capacity(capacity);
        let metadata = if tags.is_empty() {
            "NULL"
        } else {
            "frontmatter_json"
        };
        let path = if utf8 { "NULL" } else { "rel_path" };
        let mut statement = connection
            .prepare(&format!(
                "SELECT id, {metadata}, {path} FROM docs WHERE {range}"
            ))
            .map_err(db_error)?;
        let mut rows = statement
            .query(params![project, lower, upper])
            .map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            operation.check()?;
            if !utf8
                && !row
                    .get_ref(2)
                    .map_err(db_error)?
                    .as_str()
                    .map_err(|err| Error::internal_with_source("read scoped path", err))?
                    .starts_with(lower)
            {
                continue;
            }
            let raw = row.get_ref(1).map_err(db_error)?;
            if tags.is_empty() || has_tags(raw.as_str().ok(), tags) {
                // The transaction must keep preflight and copy on one snapshot.
                if documents.len() == capacity {
                    return Err(Error::internal("document scope exceeded its preflight"));
                }
                documents.push(row.get(0).map_err(db_error)?);
            }
        }
        documents.sort_unstable();
        Ok(Self {
            documents,
            _permit: permit,
        })
    }

    pub(crate) fn documents(&self) -> &[i64] {
        &self.documents
    }
}

fn overflow() -> Error {
    Error::Admission {
        message: "search scope exceeds the supported byte domain".to_owned(),
    }
}

/// Upper bound for a literal UTF-8/BINARY prefix range. Maximal suffixes
/// are removed, and the surrogate range is skipped. No LIKE/wildcards.
fn prefix_end(prefix: &str) -> Option<String> {
    let mut end = prefix.to_owned();
    while let Some(last) = end.pop() {
        let next = u32::from(last) + 1;
        if let Some(next) = char::from_u32(next).or_else(|| (next == 0xd800).then_some('\u{e000}'))
        {
            end.push(next);
            return Some(end);
        }
    }
    None
}

/// Stored malformed/non-object/non-array JSON means no tags. Non-string
/// elements are ignored, and duplicate object keys retain last-key semantics.
pub(crate) fn has_tags(raw: Option<&str>, required: &[String]) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let Some(stored) = value.get("tags").and_then(Value::as_array) else {
        return false;
    };
    required
        .iter()
        .all(|tag| stored.iter().any(|value| value.as_str() == Some(tag)))
}

/// A bounded page, hashed while row text is borrowed from `SQLite`. Caller
/// validates generation in the same read snapshot; page owns no database.
pub(crate) struct HashPage {
    pub chunks: Vec<(u64, String)>,
    pub more: bool,
}

pub(crate) fn hash_page(
    connection: &Connection,
    project: i64,
    documents: &[i64],
    after: (i64, u32),
    operation: &SearchOperation,
) -> Result<HashPage> {
    if documents.is_empty() {
        return Ok(HashPage {
            chunks: Vec::new(),
            more: false,
        });
    }
    let placeholders = std::iter::repeat_n("?", documents.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT c.doc_id,c.seq,c.text FROM chunks c JOIN docs d ON d.id=c.doc_id
        WHERE d.project_id=? AND c.doc_id IN ({placeholders}) AND (c.doc_id,c.seq)>(?,?)
        ORDER BY c.doc_id,c.seq LIMIT {}",
        BATCH_KEYS + 1
    );
    let values = std::iter::once(project)
        .chain(documents.iter().copied())
        .chain([after.0, i64::from(after.1)]);
    let mut statement = connection.prepare(&sql).map_err(db_error)?;
    let mut rows = statement
        .query(params_from_iter(values))
        .map_err(db_error)?;
    let mut page = Vec::with_capacity(BATCH_KEYS);
    let mut more = false;
    while let Some(row) = rows.next().map_err(db_error)? {
        operation.check()?;
        if page.len() == BATCH_KEYS {
            more = true;
            break;
        }
        let text = row
            .get_ref(2)
            .map_err(db_error)?
            .as_str()
            .map_err(|err| Error::internal_with_source("read scoped chunk text", err))?;
        page.push((
            chunk_id(row.get(0).map_err(db_error)?, row.get(1).map_err(db_error)?),
            chunk_hash(text),
        ));
    }
    Ok(HashPage { chunks: page, more })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::work::Cancellation;
    use crate::search::settings;
    use std::time::Instant;

    #[test]
    fn binary_range_matches_literal_prefixes_and_tolerant_tags() {
        for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
            check_encoding(encoding);
        }
    }

    fn check_encoding(encoding: &str) {
        let db = Connection::open_in_memory().expect("db");
        db.pragma_update(None, "encoding", encoding)
            .expect("encoding");
        db.execute_batch("CREATE TABLE docs(id INTEGER PRIMARY KEY,project_id INTEGER,rel_path TEXT,frontmatter_json TEXT); CREATE UNIQUE INDEX paths ON docs(project_id,rel_path);").expect("schema");
        let paths = [
            "ÿ/a",
            "Ā/a",
            "docs/a",
            "docsA",
            "Docs/a",
            "docs%/a",
            "docs_/a",
            "docs\0/a",
            "δοκ/a",
            "文/a",
            "\u{d7ff}/a",
            "\u{e000}/a",
            "\u{10ffff}/a",
            "\u{10ffff}\u{10ffff}",
        ];
        for (id, path) in paths.iter().enumerate() {
            db.execute(
                "INSERT INTO docs VALUES(?1,1,?2,?3)",
                params![
                    i64::try_from(id).expect("fixture id") + 1,
                    path,
                    r#"{"tags":["api","docs"]}"#
                ],
            )
            .expect("insert");
        }
        db.execute("INSERT INTO docs VALUES(99,2,'docs/foreign',NULL)", [])
            .expect("foreign");
        let mut policy = settings::standalone().expect("policy").into_settings();
        policy.max_retained_payload_bytes = policy.max_request_payload_bytes;
        let budget = PayloadBudget::new(policy.clone()).expect("budget");
        let operation =
            SearchOperation::new(&policy, Instant::now(), Cancellation::new()).expect("operation");
        let tags = ["api".to_owned(), "docs".to_owned()];
        for prefix in [
            "ÿ",
            "Ā",
            "",
            "docs",
            "docs/",
            "Docs",
            "docs%",
            "docs_",
            "docs\0",
            "δο",
            "文",
            "\u{d7ff}",
            "\u{10ffff}",
            "\u{10ffff}\u{10ffff}",
            "absent",
        ] {
            let scope = DocumentScope::capture(
                &db,
                ScopeRequest {
                    project: 1,
                    prefix: Some(prefix),
                    tags: &tags,
                    dense: false,
                },
                &budget,
                &operation,
            )
            .expect("scope");
            let expected: Vec<_> = paths
                .iter()
                .enumerate()
                .filter(|(_, path)| path.starts_with(prefix))
                .map(|(id, _)| i64::try_from(id).expect("fixture id") + 1)
                .collect();
            assert_eq!(scope.documents(), expected, "prefix {prefix:?}");
        }
        for (raw, expected) in [
            (None, false),
            (Some("garbage"), false),
            (Some("[]"), false),
            (Some(r#"{"tags":"api"}"#), false),
            (Some(r#"{"tags":[1,"api",null,"docs"]}"#), true),
            (Some(r#"{"tags":["api"],"tags":["api","docs"]}"#), true),
            (Some(r#"{"tags":["api","docs"],"tags":null}"#), false),
            (Some(r#"{"tags":["API","docs"]}"#), false),
        ] {
            assert_eq!(has_tags(raw, &tags), expected, "{raw:?}");
        }
        assert!(
            budget.reserve(policy.max_retained_payload_bytes).is_ok(),
            "all scope permits released"
        );
    }
}
