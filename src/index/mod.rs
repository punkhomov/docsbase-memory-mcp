//! Markdown indexing pipeline.

pub(crate) mod async_job;
pub mod chunk;
pub mod frontmatter;
pub(crate) mod generation_job;
pub mod job;
pub(crate) mod lexical_settings;
pub mod outcome;
pub mod settings;
pub mod tantivy_index;
pub mod textnorm;
pub mod tokenizer;
pub mod walk;
pub mod writer_settings;

mod query;
pub use lexical_settings::LexicalSettings;
/// Compatibility name for the resolved lexical search settings.
pub use lexical_settings::LexicalSettings as LexicalWeights;
pub use query::QueryMode;
