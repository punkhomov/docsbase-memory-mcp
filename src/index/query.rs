//! Owned query interpretation and literal AST preparation.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use tantivy::query::{Query, QueryParser};
use tantivy::query_grammar::{Delimiter, Occur, UserInputAst, UserInputLeaf, UserInputLiteral};

use super::lexical_settings::normalize_boost;
use crate::error::{Error, Result};

/// Per-request interpretation; its default preserves the existing syntax API.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryMode {
    /// Interpret the existing query language, including fields and operators.
    #[default]
    Syntax,
    /// Analyze text without giving punctuation query-language meaning.
    Literal,
}

impl QueryMode {
    /// The stable request spelling, shared by CLI, JSON and schema adapters.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::Literal => "literal",
        }
    }

    /// Validate the optional wire value once at request boundaries.
    pub(crate) fn from_argument(value: Option<&serde_json::Value>) -> Result<Self> {
        match value {
            None | Some(serde_json::Value::Null) => Ok(Self::default()),
            Some(serde_json::Value::String(value)) => value.parse().map_err(|_| Error::Protocol {
                message: "search_docs: query_mode must be syntax or literal".to_owned(),
            }),
            Some(_) => Err(Error::Protocol {
                message: "search_docs: query_mode must be syntax or literal".to_owned(),
            }),
        }
    }
}

impl fmt::Display for QueryMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for QueryMode {
    type Err = &'static str;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "syntax" => Ok(Self::Syntax),
            "literal" => Ok(Self::Literal),
            _ => Err("expected syntax or literal"),
        }
    }
}

pub(super) fn prepare_query(
    parser: &QueryParser,
    query: &str,
    mode: QueryMode,
) -> Result<Box<dyn Query>> {
    if mode == QueryMode::Syntax {
        if query.contains('^')
            && let Ok(ast) = tantivy::query_grammar::parse_query(query)
            && !finite_boosts(&ast, 1.0)
        {
            return Err(Error::Query {
                message: "query contains a non-finite lexical boost".to_owned(),
            });
        }
        return parser.parse_query(query).map_err(|error| Error::Query {
            message: format!("parse query {query:?}: {error}"),
        });
    }
    let mut seen = HashSet::new();
    let clauses = query
        .split_whitespace()
        .filter(|word| seen.insert(*word))
        .map(|word| {
            (
                Some(Occur::Should),
                UserInputAst::Leaf(Box::new(UserInputLeaf::Literal(UserInputLiteral {
                    field_name: None,
                    phrase: word.to_owned(),
                    delimiter: Delimiter::None,
                    slop: 0,
                    prefix: false,
                }))),
            )
        })
        .collect();
    let parsed = parser
        .build_query_from_user_input_ast(UserInputAst::Clause(clauses))
        .map_err(|error| Error::Query {
            message: format!("prepare literal query: {error}"),
        })?;
    let mut has_terms = false;
    parsed.query_terms(&mut |_, _| has_terms = true);
    if !has_terms {
        return Err(Error::Query {
            message: "literal query has no searchable terms".to_owned(),
        });
    }
    Ok(parsed)
}

// Tantivy multiplies outer boosts before applying the field weights. In
// particular 0.0 * infinity would become NaN and disappear from TopDocs.
#[allow(clippy::cast_possible_truncation)] // Match Tantivy, then check for overflow.
fn finite_boosts(ast: &UserInputAst, combined: f32) -> bool {
    match ast {
        UserInputAst::Clause(clauses) => {
            clauses.iter().all(|(_, ast)| finite_boosts(ast, combined))
        }
        UserInputAst::Leaf(_) => true,
        UserInputAst::Boost(ast, boost) => {
            let boost = normalize_boost(boost.into_inner() as f32);
            let combined = combined * boost;
            boost.is_finite() && combined.is_finite() && finite_boosts(ast, combined)
        }
    }
}
