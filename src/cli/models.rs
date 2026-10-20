//! `docsbase models use` — point the hybrid stages at explicit models.
//!
//! All parameters are manual by design: the command never discovers,
//! infers, or defaults model capabilities. It validates shapes locally,
//! probes exactly what was given (one embed call, one rerank call), and
//! writes the project `.docsbase.toml`. Unknown models, unsupported widths
//! and unreachable endpoints fail here with the provider's own error —
//! never later at index time.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::error::Error;
use crate::hybrid::config::{
    DEFAULT_RERANK_WINDOW, MAX_RERANK_WINDOW, MIN_RERANK_WINDOW, PROVIDER_OPENROUTER,
};

/// Model management commands.
#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// Set explicit embedding/rerank models for the current project.
    Use(UseArgs),
}

/// Explicit model selection; every model-affecting value is required from
/// the operator — nothing is looked up or defaulted from a catalog.
#[derive(Debug, Args)]
pub struct UseArgs {
    /// Embedding model id as the provider knows it.
    #[arg(long)]
    pub embedding: Option<String>,
    /// Embedding width. Required with `--embedding`: widths are
    /// model-specific (a model either truncates or rejects), so carrying
    /// over another model's width would fail late at the provider.
    #[arg(long)]
    pub dims: Option<u32>,
    /// Rerank model id as the provider knows it.
    #[arg(long)]
    pub rerank: Option<String>,
    /// Rerank candidate window.
    #[arg(long, default_value_t = DEFAULT_RERANK_WINDOW)]
    pub window: u32,
    /// Validate and print the resulting config without probing or writing.
    #[arg(long)]
    pub dry_run: bool,
    /// Project directory (default: current directory).
    #[arg(long)]
    pub path: Option<PathBuf>,
}

/// Runs the `models use` command.
///
/// # Errors
/// Returns [`Error::Admission`] for invalid arguments, a missing feature
/// build, probe failures, or filesystem errors.
pub fn run(args: &UseArgs) -> anyhow::Result<()> {
    let root = match &args.path {
        Some(path) => path.clone(),
        None => std::env::current_dir()
            .map_err(|err| Error::internal_with_source("resolve current directory", err))?,
    };
    let existing = read_project_toml(&root.join(".docsbase.toml"))?;
    let table = resolve_hybrid_table(existing.as_ref(), args)?;
    if args.dry_run {
        println!(
            "{}",
            toml::to_string(&table)
                .map_err(|err| { Error::internal_with_source("render hybrid config", err) })?
        );
        return Ok(());
    }
    #[cfg(not(feature = "hybrid-remote"))]
    {
        let _ = &table;
        Err(Error::Admission {
            message: "models use needs a hybrid-remote build for the probe; rebuild with --features hybrid-remote (dry-run works offline)".to_owned(),
        }
        .into())
    }
    #[cfg(feature = "hybrid-remote")]
    {
        probe_table(&table)?;
        write_project_toml(&root.join(".docsbase.toml"), existing.as_ref(), &table)?;
        print_summary(&table);
        Ok(())
    }
}

/// Reads the project file as an untyped table; missing file means "no
/// local overrides yet". Comments are not preserved on rewrite.
fn read_project_toml(path: &std::path::Path) -> anyhow::Result<Option<toml::Value>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let value: toml::Value = toml::from_str(&raw).map_err(|err| {
                Error::internal_with_source(format!("parse {}", path.display()), err)
            })?;
            Ok(Some(value))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(Error::internal_with_source(format!("read {}", path.display()), err).into())
        }
    }
}

/// Merges explicit arguments over the existing `hybrid` table (if any) and
/// validates shapes. Untouched stages and `ranking` survive; comments do
/// not (noted in the command output).
fn resolve_hybrid_table(
    existing: Option<&toml::Value>,
    args: &UseArgs,
) -> anyhow::Result<toml::Value> {
    if args.embedding.is_none() && args.rerank.is_none() {
        return Err(Error::Admission {
            message: "nothing to set: pass --embedding and/or --rerank".to_owned(),
        }
        .into());
    }
    if args.embedding.is_some() && args.dims.is_none() {
        return Err(Error::Admission {
            message: "setting --embedding requires explicit --dims (widths are model-specific)"
                .to_owned(),
        }
        .into());
    }
    if let Some(dims) = args.dims {
        if dims == 0 {
            return Err(Error::Admission {
                message: "--dims must be positive".to_owned(),
            }
            .into());
        }
    }
    for model in [&args.embedding, &args.rerank].into_iter().flatten() {
        if model.trim().is_empty() {
            return Err(Error::Admission {
                message: "model ids must be non-empty".to_owned(),
            }
            .into());
        }
    }
    if !(MIN_RERANK_WINDOW..=MAX_RERANK_WINDOW).contains(&args.window) {
        return Err(Error::Admission {
            message: format!("--window must be {MIN_RERANK_WINDOW}..={MAX_RERANK_WINDOW}"),
        }
        .into());
    }
    let mut hybrid = match existing.and_then(|root| root.get("hybrid")).cloned() {
        Some(toml::Value::Table(table)) => table,
        // Bool shorthand or absent: start from an enabled empty table;
        // ranking falls back to the file default downstream.
        _ => toml::map::Map::new(),
    };
    hybrid.insert("enabled".to_owned(), toml::Value::Boolean(true));
    if let (Some(model), Some(dims)) = (&args.embedding, args.dims) {
        let mut stage = toml::map::Map::new();
        stage.insert(
            "provider".to_owned(),
            toml::Value::String(PROVIDER_OPENROUTER.to_owned()),
        );
        stage.insert("model".to_owned(), toml::Value::String(model.clone()));
        stage.insert(
            "dimensions".to_owned(),
            toml::Value::Integer(i64::from(dims)),
        );
        stage.insert("normalize".to_owned(), toml::Value::Boolean(true));
        hybrid.insert("embedding".to_owned(), toml::Value::Table(stage));
    }
    if let Some(model) = &args.rerank {
        let mut stage = toml::map::Map::new();
        stage.insert(
            "provider".to_owned(),
            toml::Value::String(PROVIDER_OPENROUTER.to_owned()),
        );
        stage.insert("model".to_owned(), toml::Value::String(model.clone()));
        stage.insert(
            "window".to_owned(),
            toml::Value::Integer(i64::from(args.window)),
        );
        hybrid.insert("reranker".to_owned(), toml::Value::Table(stage));
    }
    Ok(toml::Value::Table(hybrid))
}

/// Probes exactly what was configured: one embed call (vector count and
/// width must match) and, when a reranker is set, one rerank call over two
/// documents. Provider rejections surface here, not at index time.
#[cfg(feature = "hybrid-remote")]
fn probe_table(table: &toml::Value) -> anyhow::Result<()> {
    use crate::hybrid::blocking;
    use crate::hybrid::openrouter::{OpenRouterEmbedder, OpenRouterReranker};
    use crate::hybrid::{EmbeddingCfg, RerankerCfg};
    use crate::vector_cache::Embedder;

    let hybrid = table
        .get("hybrid")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| Error::internal("resolved config has no hybrid table"))?;
    let outcome = blocking::enter(|| {
        if let Some(stage) = hybrid.get("embedding").and_then(toml::Value::as_table) {
            let cfg = EmbeddingCfg {
                provider: PROVIDER_OPENROUTER.to_owned(),
                model: table_string(stage, "model")?,
                dimensions: table_int(stage, "dimensions")?,
                normalize: true,
            };
            let embedder = OpenRouterEmbedder::new(cfg)?;
            let vectors = embedder.embed(&["probe".to_owned()])?;
            if vectors.len() != 1 {
                return Err(Error::internal(format!(
                    "probe embed returned {} vectors for one input",
                    vectors.len()
                )));
            }
            let width = vectors[0].len();
            let want = table_int(stage, "dimensions")? as usize;
            if width != want {
                return Err(Error::internal(format!(
                    "probe embed returned width {width}, want {want}"
                )));
            }
        }
        if let Some(stage) = hybrid.get("reranker").and_then(toml::Value::as_table) {
            let cfg = RerankerCfg {
                provider: PROVIDER_OPENROUTER.to_owned(),
                model: table_string(stage, "model")?,
                window: table_int(stage, "window")?,
            };
            let reranker = OpenRouterReranker::new(cfg)?;
            let ranked = crate::hybrid::Reranker::rerank(
                &reranker,
                "probe",
                &["alpha".to_owned(), "beta".to_owned()],
                2,
            )?;
            if ranked.len() != 2 {
                return Err(Error::internal(format!(
                    "probe rerank returned {} scores for two documents",
                    ranked.len()
                )));
            }
        }
        Ok(())
    });
    Ok(outcome?)
}

#[cfg(feature = "hybrid-remote")]
fn table_string(
    stage: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> crate::error::Result<String> {
    stage
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::internal(format!("resolved config lacks hybrid.{key}")))
}

#[cfg(feature = "hybrid-remote")]
fn table_int(stage: &toml::map::Map<String, toml::Value>, key: &str) -> crate::error::Result<u32> {
    let value = stage
        .get(key)
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| Error::internal(format!("resolved config lacks hybrid.{key}")))?;
    u32::try_from(value).map_err(|_| Error::internal(format!("hybrid.{key} out of range")))
}

/// Merges the resolved `hybrid` table into the project file, preserving
/// every other key. Comments are not preserved by the TOML round-trip.
#[cfg(feature = "hybrid-remote")]
fn write_project_toml(
    path: &std::path::Path,
    existing: Option<&toml::Value>,
    table: &toml::Value,
) -> anyhow::Result<()> {
    let mut root = existing
        .cloned()
        .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
    if let (Some(map), Some(hybrid)) = (root.as_table_mut(), table.get("hybrid")) {
        map.insert("hybrid".to_owned(), hybrid.clone());
    }
    let rendered =
        toml::to_string(&root).map_err(|err| Error::internal_with_source("render config", err))?;
    std::fs::write(path, rendered)
        .map_err(|err| Error::internal_with_source(format!("write {}", path.display()), err))?;
    Ok(())
}

/// Prints pins, storage estimate and the reload note.
#[cfg(feature = "hybrid-remote")]
fn print_summary(table: &toml::Value) {
    use crate::hybrid::EmbeddingCfg;
    use crate::hybrid::config::cache_model_id;

    if let Some(stage) = table
        .get("hybrid")
        .and_then(|hybrid| hybrid.get("embedding"))
        .and_then(toml::Value::as_table)
    {
        let dims = stage
            .get("dimensions")
            .and_then(toml::Value::as_integer)
            .unwrap_or_default();
        let model = stage
            .get("model")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        let pin = cache_model_id(&EmbeddingCfg {
            provider: PROVIDER_OPENROUTER.to_owned(),
            model: model.to_owned(),
            dimensions: u32::try_from(dims).unwrap_or_default(),
            normalize: true,
        });
        println!("embedding pin: {pin} (~{} bytes/chunk)", dims * 4);
    }
    if let Some(stage) = table
        .get("hybrid")
        .and_then(|hybrid| hybrid.get("reranker"))
        .and_then(toml::Value::as_table)
    {
        let model = stage
            .get("model")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        println!("rerank pin: {PROVIDER_OPENROUTER}/{model}");
    }
    println!("comments in .docsbase.toml are not preserved by this rewrite;");
    println!("applies on the next project open (restart a running detached daemon).");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(embedding: Option<&str>, dims: Option<u32>, rerank: Option<&str>) -> UseArgs {
        UseArgs {
            embedding: embedding.map(str::to_owned),
            dims,
            rerank: rerank.map(str::to_owned),
            window: DEFAULT_RERANK_WINDOW,
            dry_run: false,
            path: None,
        }
    }

    fn parse(toml_text: &str) -> toml::Value {
        toml::from_str(toml_text).expect("test TOML parses")
    }

    #[test]
    fn fresh_file_gets_enabled_table() {
        let table = resolve_hybrid_table(None, &args(Some("m/e"), Some(512), Some("m/r")))
            .expect("resolve");
        assert_eq!(
            table.get("enabled").and_then(toml::Value::as_bool),
            Some(true)
        );
        let embedding = table.get("embedding").expect("embedding");
        assert_eq!(
            embedding.get("model").and_then(toml::Value::as_str),
            Some("m/e")
        );
        assert_eq!(
            embedding
                .get("dimensions")
                .and_then(toml::Value::as_integer),
            Some(512)
        );
        assert_eq!(
            table
                .get("reranker")
                .and_then(|reranker| reranker.get("window"))
                .and_then(toml::Value::as_integer),
            Some(50)
        );
    }

    #[test]
    fn untouched_stages_and_ranking_survive() {
        let existing = parse(
            "max_file_size = 1\n[hybrid]\nranking = \"rrf\"\n[hybrid.reranker]\nprovider = \"openrouter\"\nmodel = \"old/r\"\nwindow = 20\n",
        );
        let table = resolve_hybrid_table(Some(&existing), &args(Some("new/e"), Some(256), None))
            .expect("resolve");
        assert_eq!(
            table.get("ranking").and_then(toml::Value::as_str),
            Some("rrf")
        );
        let reranker = table.get("reranker").expect("reranker kept");
        assert_eq!(
            reranker.get("model").and_then(toml::Value::as_str),
            Some("old/r")
        );
        assert_eq!(
            table
                .get("embedding")
                .and_then(|embedding| embedding.get("dimensions"))
                .and_then(toml::Value::as_integer),
            Some(256)
        );
    }

    #[test]
    fn bool_shorthand_becomes_table() {
        let existing = parse("[hybrid]\nenabled = true\n");
        let table = resolve_hybrid_table(Some(&existing), &args(Some("m/e"), Some(128), None))
            .expect("resolve");
        assert!(table.is_table());
    }

    #[test]
    fn embedding_requires_dims() {
        let err =
            resolve_hybrid_table(None, &args(Some("m/e"), None, None)).expect_err("dims required");
        assert!(err.to_string().contains("--dims"));
    }

    #[test]
    fn rejects_shapes() {
        assert!(
            resolve_hybrid_table(None, &args(None, None, None)).is_err(),
            "nothing to set"
        );
        assert!(
            resolve_hybrid_table(None, &args(Some("m/e"), Some(0), None)).is_err(),
            "zero dims"
        );
        assert!(
            resolve_hybrid_table(None, &args(Some("  "), Some(8), None)).is_err(),
            "blank model"
        );
        let mut bad_window = args(None, None, Some("m/r"));
        bad_window.window = 5;
        assert!(
            resolve_hybrid_table(None, &bad_window).is_err(),
            "window below minimum"
        );
    }
}
