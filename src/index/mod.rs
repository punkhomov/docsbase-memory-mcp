//! Markdown indexing pipeline.

pub(crate) mod async_job;
pub mod chunk;
pub mod frontmatter;
pub mod job;
pub(crate) mod lexical_settings;
pub mod outcome;
pub mod tantivy_index;
pub mod textnorm;
pub mod tokenizer;
pub mod walk;

mod query;
pub use lexical_settings::LexicalWeights;
pub use query::QueryMode;
