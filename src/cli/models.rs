//! `docsbase models use embed|rerank` — point one hybrid stage at an
//! explicit model (FR-8). Purpose-first: one subcommand per stage, one
//! model per invocation, options scoped to the purpose. Scope: project
//! `.docsbase.toml` by default (or `--local`); `--global` writes the
//! machine config and is required outside a project; `--path` overrides
//! the project root and cannot combine with `--global`. Provider inference:
//! a registry name implies `fastembed`; any other name requires
//! `--provider`. Probes: fastembed offline against the cached model;
//! `OpenRouter` single-shot (no retry wrapper). Unknown models, unsupported
//! widths and unreachable providers fail here — never later at index time.

#[cfg(feature = "hybrid-local")]
use std::path::Path;
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::error::Error;
use crate::hybrid::config::{
    DEFAULT_RERANK_WINDOW, MAX_RERANK_WINDOW, MIN_RERANK_WINDOW, PROVIDER_FASTEMBED,
    PROVIDER_OPENROUTER,
};

/// Model management commands.
#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// Set an explicit embedding/rerank model for the current project.
    Use(UseArgs),
    /// Fetch a pinned local model revision (`hybrid-local`).
    #[cfg(feature = "hybrid-local")]
    Fetch(FetchArgs),
    /// List registry entries and their cache status (`hybrid-local`).
    #[cfg(feature = "hybrid-local")]
    List,
    /// Remove all cached revisions of a model (`hybrid-local`, FR-7).
    #[cfg(feature = "hybrid-local")]
    Remove(RemoveArgs),
}

/// Arguments for `models fetch` (FR-6).
#[cfg(feature = "hybrid-local")]
#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Registry name of the model to fetch.
    pub name: String,
    /// Re-download even when the cached revision verifies.
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `models remove` (FR-7).
#[cfg(feature = "hybrid-local")]
#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Registry name of the model to remove.
    pub name: String,
    /// Delete even while a loaded instance holds the shared cache lock.
    #[arg(long)]
    pub force: bool,
}

/// Purpose-first model selection (FR-8): one stage per invocation, with
/// options scoped to the purpose. Shared flags are global so they parse
/// after the subcommand, e.g. `models use embed <model> --dry-run`.
#[derive(Debug, Args)]
pub struct UseArgs {
    #[command(subcommand)]
    pub purpose: UsePurpose,
    /// Validate and print the resulting config without probing or writing.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Project directory (default: current directory).
    #[arg(long, global = true)]
    pub path: Option<PathBuf>,
    /// Write the global config (defaults for new projects) instead of the
    /// project file. Required outside a project.
    #[arg(long, global = true)]
    pub global: bool,
    /// Explicitly target the project file (default inside a project).
    #[arg(long, global = true)]
    pub local: bool,
}

/// One stage per invocation; setting both stages in one call is impossible
/// by CLI shape (no `--embedding`/`--rerank` mixing).
#[derive(Debug, Subcommand)]
pub enum UsePurpose {
    /// Pin the embedding stage (`[hybrid.embedding]`).
    Embed(EmbedArgs),
    /// Pin the rerank stage (`[hybrid.reranker]`).
    Rerank(RerankArgs),
}

/// Embedding stage arguments.
#[derive(Debug, Args)]
pub struct EmbedArgs {
    /// Model id as the provider knows it; a registry name implies fastembed.
    pub model: String,
    /// Provider override; required when the model is not a registry entry.
    #[arg(long)]
    pub provider: Option<String>,
    /// Embedding width. Optional for fastembed (inferred from the registry);
    /// required for `OpenRouter`, which has no catalog.
    #[arg(long)]
    pub dims: Option<u32>,
}

/// Rerank stage arguments.
#[derive(Debug, Args)]
pub struct RerankArgs {
    /// Model id as the provider knows it; a registry name implies fastembed.
    pub model: String,
    /// Provider override; required when the model is not a registry entry.
    #[arg(long)]
    pub provider: Option<String>,
    /// Rerank candidate window.
    #[arg(long)]
    pub window: Option<u32>,
}

/// Runs `models fetch <name>`: looks up the registry, downloads every pinned
/// file with verification and installs atomically (FR-6). The only network
/// step in the model lifecycle; search/index never call it.
///
/// # Errors
/// Returns [`Error::Admission`] for unknown names, transport failures,
/// hash/size mismatches, cap violations and preflight failures.
#[cfg(feature = "hybrid-local")]
pub fn fetch_cmd(args: &FetchArgs) -> anyhow::Result<()> {
    use crate::hybrid::models;
    let entry = models::entry(&args.name)?;
    let source = models::HttpModelSource::new()?;
    let (dir, installed) = models::fetch(entry, &source, args.force)?;
    println!(
        "fetched {} (revision {}) -> {}",
        entry.name,
        entry.revision,
        dir.display()
    );
    // A running daemon caches loaded instances per stage; whenever a
    // revision was actually placed on disk (staging → promote, or
    // `--force` reinstall), it keeps serving the old instance until
    // restarted (round-3 muse-3 / round-4 C5). A verified-complete fast
    // path changes nothing on disk, so no note.
    if installed || args.force {
        println!("note: restart a running detached daemon to pick up the new revision");
    }
    Ok(())
}

/// Runs `models list`: prints the registry with cache state, sizes, the
/// provider states and the currently selected pins (FR-7).
///
/// # Errors
/// Returns an internal error when verifying a cached revision fails.
#[cfg(feature = "hybrid-local")]
pub fn list_cmd() -> anyhow::Result<()> {
    use crate::hybrid::models;

    // Provider states (FR-11) and the selected pins (FR-9) come from the
    // merged config (global + project).
    let global = crate::config::Config::global()?;
    let (embed_pin, rerank_pin) = selected_pins();
    let fastembed_state =
        crate::hybrid::config::provider_status(&global.providers, PROVIDER_FASTEMBED);
    let openrouter_state =
        crate::hybrid::config::provider_status(&global.providers, PROVIDER_OPENROUTER);
    println!(
        "providers:\tfastembed={}\topenrouter={}",
        state_label(fastembed_state.state),
        state_label(openrouter_state.state)
    );
    println!(
        "selected:\tembed={}\trerank={}",
        embed_pin.as_deref().unwrap_or("-"),
        rerank_pin.as_deref().unwrap_or("-")
    );
    for entry in models::REGISTRY {
        // Shallow per-file size check (no full hashing per invocation;
        // round-3 deepseek-7).
        let state = if models::cached_shallow(entry)? {
            "fetched"
        } else {
            "missing"
        };
        let cached_bytes = if state == "fetched" {
            dir_size(&models::cache_dir(entry.name, entry.revision))
        } else {
            0
        };
        println!(
            "{}\t{}\t{}\t{state}\t{} on disk",
            entry.name,
            task_label(entry.task),
            human_bytes(entry.total_size()),
            human_bytes(cached_bytes)
        );
    }
    Ok(())
}

/// Runs `models remove <name>`: removes every cached revision of the model
/// (FR-7). Refuses while a loaded instance holds the shared cache lock,
/// unless `--force` is given (then it deletes anyway with a warning; a
/// running process keeps its open handles — on Windows the OS may still
/// refuse to delete a mapped file, which surfaces here).
///
/// # Errors
/// Admission for unknown names, missing caches, or a live shared lock.
#[cfg(feature = "hybrid-local")]
pub fn remove_cmd(args: &RemoveArgs) -> anyhow::Result<()> {
    use crate::hybrid::models;

    let entry = models::entry(&args.name)?;
    // Every cached revision is removed (FR-7), including stale revisions
    // from an older registry pin and partial installs: gate on the model's
    // directory, not on the currently pinned revision directory.
    let pinned_dir = models::cache_dir(entry.name, entry.revision);
    let model_root = match pinned_dir.parent() {
        Some(parent) => parent.to_path_buf(),
        None => {
            return Err(Error::internal("cache path has no parent").into());
        }
    };
    if !model_root.exists() {
        return Err(Error::Admission {
            message: format!(
                "model \"{}\" is not fetched (nothing to remove); run `docsbase models fetch {}`",
                entry.name, entry.name
            ),
        }
        .into());
    }

    // Exclusive, non-blocking probe of the model-root `.lock` (the lock
    // lives at the root, shared by loaded instances for their lifetime and
    // held exclusively by an in-flight fetch — including its `.staging-*`
    // phase, round-2 D2/D3). Contention → Admission unless `--force`.
    let lock_path = model_root.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| Error::internal_with_source(format!("open {}", lock_path.display()), err))?;
    // Leaked `'static` lock, same pattern as `take_shared_lock`;
    // removal happens once per process invocation, so the leak is
    // bounded and deliberate (matches the shared-lock design).
    let lock: &'static mut fd_lock::RwLock<std::fs::File> =
        Box::leak(Box::new(fd_lock::RwLock::new(file)));
    let guard = match lock.try_write() {
        Ok(guard) => Some(guard),
        Err(_) if args.force => {
            eprintln!(
                "warning: {} is loaded/in use (shared lock held); --force deletes anyway",
                entry.name
            );
            None
        }
        Err(_) => {
            return Err(Error::Admission {
                message: format!(
                    "model \"{}\" is loaded/in use; stop the daemon (or pass --force) then retry",
                    entry.name
                ),
            }
            .into());
        }
    };

    // Keep the guard alive while deleting (Linux/macOS tolerate it; on
    // Windows the OS may refuse to delete an open/mapped file — surface it).
    std::fs::remove_dir_all(&model_root).map_err(|err| {
        Error::internal_with_source(format!("remove {}", model_root.display()), err)
    })?;
    drop(guard);
    println!("removed {} (all cached revisions)", entry.name);
    Ok(())
}

/// Depth-first byte count of a directory (cache sizes for `models list`).
#[cfg(feature = "hybrid-local")]
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else if let Ok(metadata) = std::fs::metadata(&path) {
                total += metadata.len();
            }
        }
    }
    total
}

/// Provider state label for `models list`.
#[cfg(feature = "hybrid-local")]
fn state_label(state: crate::hybrid::config::ProviderState) -> &'static str {
    use crate::hybrid::config::ProviderState;
    match state {
        ProviderState::Unknown => "unknown",
        ProviderState::NotConnected => "not-connected",
        ProviderState::Connected => "connected",
        ProviderState::Broken => "broken",
    }
}

/// Selected hybrid pins from the merged config (global + project), FR-9.
#[cfg(feature = "hybrid-local")]
fn selected_pins() -> (Option<String>, Option<String>) {
    use crate::hybrid::EmbeddingCfg;
    use crate::hybrid::config::embedding_model_id;

    let mut embed = None;
    let mut rerank = None;
    let mut collect = |root: &toml::Value| {
        let Some(hybrid) = root.get("hybrid").and_then(toml::Value::as_table) else {
            return;
        };
        if let Some(stage) = hybrid.get("embedding").and_then(toml::Value::as_table) {
            let (Some(provider), Some(model), Some(dims)) = (
                stage.get("provider").and_then(toml::Value::as_str),
                stage.get("model").and_then(toml::Value::as_str),
                stage.get("dimensions").and_then(toml::Value::as_integer),
            ) else {
                return;
            };
            let normalize = stage
                .get("normalize")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            embed = Some(embedding_model_id(&EmbeddingCfg {
                provider: provider.to_owned(),
                model: model.to_owned(),
                dimensions: u32::try_from(dims).unwrap_or_default(),
                normalize,
            }));
        }
        if let Some(stage) = hybrid.get("reranker").and_then(toml::Value::as_table) {
            let (Some(provider), Some(model)) = (
                stage.get("provider").and_then(toml::Value::as_str),
                stage.get("model").and_then(toml::Value::as_str),
            ) else {
                return;
            };
            let window = stage
                .get("window")
                .and_then(toml::Value::as_integer)
                .unwrap_or(i64::from(DEFAULT_RERANK_WINDOW));
            let window = u32::try_from(window).unwrap_or(DEFAULT_RERANK_WINDOW);
            rerank = Some(format!("{provider}/{model}@w{window}"));
        }
    };
    if let Ok(Some(root)) = read_global_toml() {
        collect(&root);
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(Some(root)) = read_project_toml(&cwd.join(".docsbase.toml")) {
            collect(&root);
        }
    }
    (embed, rerank)
}

#[cfg(feature = "hybrid-local")]
fn task_label(task: crate::hybrid::models::Task) -> &'static str {
    match task {
        crate::hybrid::models::Task::Embedding => "embedding",
        crate::hybrid::models::Task::Rerank => "rerank",
    }
}

#[cfg(feature = "hybrid-local")]
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024 && unit < UNITS.len() - 1 {
        value /= 1024;
        unit += 1;
    }
    format!("{value} {}", UNITS[unit])
}

/// Runs the `models use` command (FR-8).
///
/// # Errors
/// Returns [`Error::Admission`] for invalid arguments, a missing feature
/// build, probe failures, or filesystem errors.
pub fn run(args: &UseArgs) -> anyhow::Result<()> {
    let scope = resolve_scope(args)?;

    let (stage_key, stage, probe) = match &args.purpose {
        UsePurpose::Embed(embed) => {
            let (provider, dims) = resolve_embed_stage(embed)?;
            let model = embed.model.trim().to_owned();
            let mut stage = toml::map::Map::new();
            stage.insert("provider".to_owned(), toml::Value::String(provider.clone()));
            stage.insert("model".to_owned(), toml::Value::String(model.clone()));
            stage.insert(
                "dimensions".to_owned(),
                toml::Value::Integer(i64::from(dims)),
            );
            stage.insert("normalize".to_owned(), toml::Value::Boolean(true));
            (
                "embedding",
                stage,
                ProbeSpec {
                    provider,
                    model,
                    dims: Some(dims),
                    window: None,
                },
            )
        }
        UsePurpose::Rerank(rerank) => {
            let (provider, window) = resolve_rerank_stage(rerank)?;
            let model = rerank.model.trim().to_owned();
            let mut stage = toml::map::Map::new();
            stage.insert("provider".to_owned(), toml::Value::String(provider.clone()));
            stage.insert("model".to_owned(), toml::Value::String(model.clone()));
            stage.insert("window".to_owned(), toml::Value::Integer(i64::from(window)));
            (
                "reranker",
                stage,
                ProbeSpec {
                    provider,
                    model,
                    dims: None,
                    window: Some(window),
                },
            )
        }
    };

    // The pre-lock snapshot is only for advisory output (notes, dry-run
    // preview). The real read-merge-write re-runs under the inter-process
    // lock, so two concurrent `models use` writers cannot lost-update each
    // other's stages (panel round-2 P1).
    let existing = match &scope {
        Scope::Project(root) => read_project_toml(&root.join(".docsbase.toml"))?,
        Scope::Global => read_global_toml()?,
    };
    let table = merge_stage(existing.as_ref(), stage_key, stage.clone())?;
    if let Some(note) = free_route_note(&table) {
        eprintln!("{note}");
    }
    if stage_key == "embedding" {
        if let Some(note) = repin_embedding_note(existing.as_ref(), &table) {
            eprintln!("{note}");
        }
    }
    if args.dry_run {
        // Preview the full merged file exactly as the write would produce it
        // (unrelated keys preserved; panel C12).
        let preview = preview_merged(existing.as_ref(), &table);
        println!(
            "{}",
            toml::to_string(&preview)
                .map_err(|err| Error::internal_with_source("render hybrid config", err))?
        );
        return Ok(());
    }

    probe_stage(&probe)?;

    let path = match &scope {
        Scope::Project(root) => root.join(".docsbase.toml"),
        Scope::Global => global_config_file()?,
    };
    // Read-merge-write under `with_config_write_lock` (C13): `stage` is
    // re-merged against the file state observed under the lock, so two
    // concurrent `models use` writers cannot lost-update each other.
    let finalized = apply_stage_locked(&path, stage_key, stage)?;
    print_summary(&finalized);
    Ok(())
}

/// Applies one stage to a config file under the inter-process write lock:
/// re-reads the current file inside the lock, merges, writes, returns the
/// merged table (panel round-2 P1: read-merge-write must be atomic).
fn apply_stage_locked(
    path: &std::path::Path,
    stage_key: &str,
    stage: toml::map::Map<String, toml::Value>,
) -> anyhow::Result<toml::Value> {
    crate::config::paths::with_config_write_lock(path, || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::internal_with_source(format!("create {}", parent.display()), err)
            })?;
        }
        let current = read_config_table(path)?;
        let table = merge_stage(current.as_ref(), stage_key, stage)?;
        write_project_toml(path, current.as_ref(), &table)?;
        Ok(table)
    })
}

/// Reads a config file as an untyped table by path (project or global).
fn read_config_table(path: &std::path::Path) -> anyhow::Result<Option<toml::Value>> {
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

/// Where `models use` writes: the project `.docsbase.toml` or the machine
/// config (FR-8).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    /// Project root holding `.docsbase.toml`.
    Project(PathBuf),
    /// Machine-wide `config.toml`.
    Global,
}

/// Resolves the write scope (FR-8): `--global` targets the machine config
/// and is required outside a project; both `--local`/`--global` error;
/// `--path` overrides the project root and cannot combine with `--global`.
///
/// # Errors
/// Admission for flag conflicts or outside-project use without `--global`.
fn resolve_scope(args: &UseArgs) -> anyhow::Result<Scope> {
    if args.local && args.global {
        return Err(Error::Admission {
            message: "--local and --global are mutually exclusive".to_owned(),
        }
        .into());
    }
    if args.path.is_some() && args.global {
        return Err(Error::Admission {
            message: "--path cannot be combined with --global (the global config lives in DOCSBASE_CONFIG_DIR)"
                .to_owned(),
        }
        .into());
    }
    if args.global {
        return Ok(Scope::Global);
    }
    let root = match &args.path {
        Some(path) => path.clone(),
        None => std::env::current_dir()
            .map_err(|err| Error::internal_with_source("resolve current directory", err))?,
    };
    if !root.join(".docsbase.toml").exists() {
        return Err(Error::Admission {
            message: format!(
                "not inside a project: {} has no .docsbase.toml; pass --global to write the machine config",
                root.display()
            ),
        }
        .into());
    }
    Ok(Scope::Project(root))
}

/// The hybrid stage a purpose selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Segment {
    Embedding,
    Rerank,
}

impl Segment {
    fn label(self) -> &'static str {
        match self {
            Segment::Embedding => "embedding",
            Segment::Rerank => "rerank",
        }
    }
}

/// Infers the provider: a registry name implies `fastembed` (purpose must
/// match the row's task); any other name needs an explicit `--provider`
/// (`OpenRouter` has no catalog, FR-8).
///
/// # Errors
/// Admission for a registry row whose task does not match the purpose, or
/// for an unknown name without `--provider`.
fn resolve_provider(
    segment: Segment,
    model: &str,
    explicit: Option<&str>,
) -> anyhow::Result<String> {
    #[cfg(feature = "hybrid-local")]
    {
        use crate::hybrid::models::{self, Task};
        if let Ok(entry) = models::entry(model) {
            let expected = match segment {
                Segment::Embedding => Task::Embedding,
                Segment::Rerank => Task::Rerank,
            };
            if entry.task != expected {
                return Err(Error::Admission {
                    message: format!(
                        "\"{model}\" is a {} model, not for {}; pick from `docsbase models list`",
                        task_label(entry.task),
                        segment.label()
                    ),
                }
                .into());
            }
            // A registry model is always fastembed; an explicit conflicting
            // `--provider` is an error, not a silent override (panel C7).
            if let Some(provider) = explicit {
                if provider != PROVIDER_FASTEMBED {
                    return Err(Error::Admission {
                        message: format!(
                            "\"{model}\" is a local registry model ({PROVIDER_FASTEMBED}); --provider {provider} conflicts — remove it"
                        ),
                    }
                    .into());
                }
            }
            return Ok(PROVIDER_FASTEMBED.to_owned());
        }
    }
    if let Some(provider) = explicit {
        return match provider {
            PROVIDER_FASTEMBED | PROVIDER_OPENROUTER => Ok(provider.to_owned()),
            other => Err(Error::Admission {
                message: format!(
                    "unknown provider \"{other}\"; want {PROVIDER_FASTEMBED} or {PROVIDER_OPENROUTER}"
                ),
            }
            .into()),
        };
    }
    Err(Error::Admission {
        message: format!(
            "\"{model}\" is not a local registry entry for {}; pass --provider (for example {PROVIDER_OPENROUTER})",
            segment.label()
        ),
    }
    .into())
}

/// Resolves the embedding stage into `(provider, dimensions)`.
///
/// # Errors
/// Admission for unknown models, wrong-task rows, dims mismatch with the
/// registry, and missing dims for non-fastembed providers.
fn resolve_embed_stage(args: &EmbedArgs) -> anyhow::Result<(String, u32)> {
    let model = args.model.trim();
    let provider = resolve_provider(Segment::Embedding, model, args.provider.as_deref())?;
    let dims = resolve_dims(model, &provider, args.dims)?;
    Ok((provider, dims))
}

/// Resolves the rerank stage into `(provider, window)`.
///
/// # Errors
/// Admission for unknown models, wrong-task rows and out-of-range windows.
fn resolve_rerank_stage(args: &RerankArgs) -> anyhow::Result<(String, u32)> {
    let model = args.model.trim();
    let provider = resolve_provider(Segment::Rerank, model, args.provider.as_deref())?;
    let window = resolve_window(args.window)?;
    Ok((provider, window))
}

/// Embedding width: fastembed reads the registry row (an optional
/// `--dims` must match); other providers require an explicit `--dims`.
///
/// # Errors
/// Admission for dims mismatch, zero dims, or a missing required `--dims`.
fn resolve_dims(model: &str, provider: &str, dims: Option<u32>) -> anyhow::Result<u32> {
    if provider == PROVIDER_FASTEMBED {
        #[cfg(feature = "hybrid-local")]
        {
            use crate::hybrid::models;
            let entry = models::entry(model)?;
            let registry_dims = entry.dim.ok_or_else(|| Error::Admission {
                message: format!(
                    "\"{model}\" is not an embedding model; pick from `docsbase models list`"
                ),
            })?;
            let registry_dims = u32::try_from(registry_dims)
                .map_err(|_| Error::internal("registry dimensions out of range"))?;
            return match dims {
                Some(given) if given != registry_dims => Err(Error::Admission {
                    message: format!(
                        "--dims {given} does not match the registry ({registry_dims}); the registry row is authoritative"
                    ),
                }
                .into()),
                Some(given) => Ok(given),
                None => Ok(registry_dims),
            };
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            let _ = (model, dims);
            return Err(Error::Admission {
                message:
                    "fastembed needs the hybrid-local build; rebuild with --features hybrid-local"
                        .to_owned(),
            }
            .into());
        }
    }
    match dims {
        Some(dims) if dims > 0 => Ok(dims),
        Some(_) => Err(Error::Admission {
            message: "--dims must be positive".to_owned(),
        }
        .into()),
        None => Err(Error::Admission {
            message: format!(
                "--dims is required for {provider} (no catalog; widths are model-specific)"
            ),
        }
        .into()),
    }
}

/// Rerank window with bounds checking; default when omitted.
///
/// # Errors
/// Admission for out-of-range windows.
fn resolve_window(window: Option<u32>) -> anyhow::Result<u32> {
    match window {
        None => Ok(DEFAULT_RERANK_WINDOW),
        Some(value) if (MIN_RERANK_WINDOW..=MAX_RERANK_WINDOW).contains(&value) => Ok(value),
        Some(value) => Err(Error::Admission {
            message: format!(
                "--window must be {MIN_RERANK_WINDOW}..={MAX_RERANK_WINDOW} (got {value})"
            ),
        }
        .into()),
    }
}

/// Merges the selected stage into the existing `hybrid` table, preserving
/// every other stage and key (FR-8). Validates the merged result, so a
/// stale bad value in the untouched stage fails here with a fix hint.
fn merge_stage(
    existing: Option<&toml::Value>,
    stage_key: &str,
    stage: toml::map::Map<String, toml::Value>,
) -> anyhow::Result<toml::Value> {
    let mut hybrid = match existing.and_then(|root| root.get("hybrid")).cloned() {
        Some(toml::Value::Table(table)) => table,
        // Bool shorthand or absent: start from an enabled empty table;
        // ranking falls back to the file default downstream.
        _ => toml::map::Map::new(),
    };
    hybrid.insert("enabled".to_owned(), toml::Value::Boolean(true));
    hybrid.insert(stage_key.to_owned(), toml::Value::Table(stage));
    validate_hybrid_table(&hybrid)?;
    let mut root = toml::map::Map::new();
    root.insert("hybrid".to_owned(), toml::Value::Table(hybrid));
    Ok(toml::Value::Table(root))
}

/// The merged file root the write path would persist: the existing root
/// with its `hybrid` table replaced by the resolved one. When no file
/// exists yet, the resolved table is the whole preview.
fn preview_merged(existing: Option<&toml::Value>, table: &toml::Value) -> toml::Value {
    match existing {
        Some(root) => {
            let mut merged = root.clone();
            if let Some(map) = merged.as_table_mut() {
                let hybrid = table
                    .get("hybrid")
                    .cloned()
                    .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
                map.insert("hybrid".to_owned(), hybrid);
            }
            merged
        }
        None => table.clone(),
    }
}

/// Validates the merged `hybrid` table: flag arguments were checked on the
/// way in, but preserved stages pass through untouched — a stale window,
/// zero dims, blank model id, unknown provider or fastembed normalize=false
/// must fail here with a fix hint, not later at index time.
fn validate_hybrid_table(hybrid: &toml::map::Map<String, toml::Value>) -> anyhow::Result<()> {
    validate_embedding_stage(hybrid)?;
    validate_reranker_stage(hybrid)?;
    if let Some(ranking) = hybrid.get("ranking").and_then(toml::Value::as_str) {
        if ranking != "lexical" && ranking != "rrf" {
            return Err(Error::Admission {
                message: format!(
                    "existing hybrid.ranking {ranking:?} is unknown (want \"lexical\" or \"rrf\"); edit .docsbase.toml"
                ),
            }
            .into());
        }
    }
    Ok(())
}

/// Validates the preserved (or newly merged) embedding stage: provider
/// allowlist and the fastembed `normalize=true` contract (round-3
/// deepseek-5).
fn validate_embedding_stage(hybrid: &toml::map::Map<String, toml::Value>) -> anyhow::Result<()> {
    if let Some(stage) = hybrid.get("embedding").and_then(toml::Value::as_table) {
        let model = stage
            .get("model")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        if model.trim().is_empty() {
            return Err(Error::Admission {
                message: "existing hybrid.embedding.model is blank (run `docsbase models use embed <model>` to fix)"
                    .to_owned(),
            }
            .into());
        }
        let dims = stage
            .get("dimensions")
            .and_then(toml::Value::as_integer)
            .unwrap_or(0);
        if dims <= 0 {
            return Err(Error::Admission {
                message: "existing hybrid.embedding.dimensions must be positive (run `docsbase models use embed <model>` to fix)"
                    .to_owned(),
            }
            .into());
        }
        let provider = stage.get("provider").and_then(toml::Value::as_str);
        match provider {
            Some(PROVIDER_FASTEMBED) => {
                if !stage
                    .get("normalize")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(true)
                {
                    return Err(Error::Admission {
                        message: "existing hybrid.embedding.normalize must be true for fastembed (run `docsbase models use embed <model>` to fix)"
                            .to_owned(),
                    }
                    .into());
                }
            }
            Some(PROVIDER_OPENROUTER) => {}
            Some(other) => {
                return Err(Error::Admission {
                    message: format!(
                        "existing hybrid.embedding.provider {other:?} is unknown (want {PROVIDER_FASTEMBED} or {PROVIDER_OPENROUTER}; run `docsbase models use embed <model>` to fix)"
                    ),
                }
                .into());
            }
            None => {
                return Err(Error::Admission {
                    message: "existing hybrid.embedding.provider is missing (run `docsbase models use embed <model>` to fix)"
                        .to_owned(),
                }
                .into());
            }
        }
    }
    Ok(())
}

/// Validates the preserved (or newly merged) reranker stage: provider
/// allowlist (round-3 deepseek-5).
fn validate_reranker_stage(hybrid: &toml::map::Map<String, toml::Value>) -> anyhow::Result<()> {
    if let Some(stage) = hybrid.get("reranker").and_then(toml::Value::as_table) {
        let model = stage
            .get("model")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        if model.trim().is_empty() {
            return Err(Error::Admission {
                message: "existing hybrid.reranker.model is blank (run `docsbase models use rerank <model>` to fix)"
                    .to_owned(),
            }
            .into());
        }
        let provider = stage.get("provider").and_then(toml::Value::as_str);
        match provider {
            Some(PROVIDER_FASTEMBED | PROVIDER_OPENROUTER) => {}
            Some(other) => {
                return Err(Error::Admission {
                    message: format!(
                        "existing hybrid.reranker.provider {other:?} is unknown (want {PROVIDER_FASTEMBED} or {PROVIDER_OPENROUTER}; run `docsbase models use rerank <model>` to fix)"
                    ),
                }
                .into());
            }
            None => {
                return Err(Error::Admission {
                    message: "existing hybrid.reranker.provider is missing (run `docsbase models use rerank <model>` to fix)"
                        .to_owned(),
                }
                .into());
            }
        }
        if let Some(window) = stage.get("window").and_then(toml::Value::as_integer) {
            let valid = u32::try_from(window)
                .is_ok_and(|value| (MIN_RERANK_WINDOW..=MAX_RERANK_WINDOW).contains(&value));
            if !valid {
                return Err(Error::Admission {
                    message: format!(
                        "existing hybrid.reranker.window {window} out of range {MIN_RERANK_WINDOW}..={MAX_RERANK_WINDOW} (pass --window to fix)"
                    ),
                }
                .into());
            }
        }
    }
    Ok(())
}

/// Re-pin note (FR-13): when the embedding pin changes, existing vectors are
/// keyed by the old id and must be re-embedded; the ANN tier is invalidated
/// for the project. Pure: returns the note text (printed on the write path
/// and dry-run before any probe); `None` when nothing changed.
fn repin_embedding_note(existing: Option<&toml::Value>, table: &toml::Value) -> Option<String> {
    let old = existing.and_then(|root| {
        root.get("hybrid")
            .and_then(toml::Value::as_table)
            .and_then(|hybrid| hybrid.get("embedding"))
            .and_then(toml::Value::as_table)
    });
    let new = table
        .get("hybrid")
        .and_then(toml::Value::as_table)
        .and_then(|hybrid| hybrid.get("embedding"))
        .and_then(toml::Value::as_table);
    let pin_of = |stage: &toml::map::Map<String, toml::Value>| -> Option<String> {
        let provider = stage.get("provider").and_then(toml::Value::as_str)?;
        let model = stage.get("model").and_then(toml::Value::as_str)?;
        let dims = stage.get("dimensions").and_then(toml::Value::as_integer)?;
        let normalize = stage
            .get("normalize")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        let cfg = crate::hybrid::EmbeddingCfg {
            provider: provider.to_owned(),
            model: model.to_owned(),
            dimensions: u32::try_from(dims).ok()?,
            normalize,
        };
        Some(crate::hybrid::config::embedding_model_id(&cfg))
    };
    match (old.and_then(pin_of), new.and_then(pin_of)) {
        (Some(old_pin), Some(new_pin)) if old_pin != new_pin => Some(format!(
            "note: embedding pin changed from {old_pin} to {new_pin}; existing vectors will be re-embedded and the ANN tier invalidated"
        )),
        (None, Some(_)) => {
            Some("note: first embedding pin; backfill will index vectors".to_owned())
        }
        _ => None,
    }
}

/// One-line privacy note for free-tier routes (FR-33): hybrid itself is an
/// explicit opt-in, but the free-vs-paid distinction is stated once, on both
/// dry-run and write paths. Pure for unit tests.
fn free_route_note(table: &toml::Value) -> Option<String> {
    let hybrid = table.get("hybrid")?.as_table()?;
    let mut routes = Vec::new();
    for stage in ["embedding", "reranker"] {
        let model = hybrid
            .get(stage)
            .and_then(|value| value.get("model"))
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        if model.contains(":free") {
            routes.push(format!("{stage} ({model})"));
        }
    }
    if routes.is_empty() {
        return None;
    }
    Some(format!(
        "note: free route(s) {} may train on prompts and rotate monthly; paid pins stay available via models use",
        routes.join(", ")
    ))
}
/// What the probe validates for a resolved stage.
#[cfg_attr(
    // `dims`/`window` are only read by the OpenRouter probe (hybrid-remote).
    not(feature = "hybrid-remote"),
    allow(dead_code)
)]
struct ProbeSpec {
    provider: String,
    model: String,
    dims: Option<u32>,
    window: Option<u32>,
}

/// Probes the resolved stage before writing (FR-8): fastembed validates the
/// provider/runtime and cached model offline; `OpenRouter` performs exactly
/// one embed and one rerank over a single-shot path (no retry wrapper).
///
/// # Errors
/// Admission for missing provider/runtime/model or a rejected probe;
/// `Internal` for an incompatible response schema.
fn probe_stage(spec: &ProbeSpec) -> anyhow::Result<()> {
    match spec.provider.as_str() {
        PROVIDER_FASTEMBED => probe_fastembed_offline(&spec.model),
        PROVIDER_OPENROUTER => probe_openrouter_singleshot(spec),
        other => Err(Error::Admission {
            message: format!(
                "unknown provider \"{other}\"; want {PROVIDER_FASTEMBED} or {PROVIDER_OPENROUTER}"
            ),
        }
        .into()),
    }
}

/// Offline fastembed probe: the runtime must resolve (connect hint) and the
/// model must be cached (fetch hint). No network (FR-8, SC-5).
///
/// # Errors
/// Admission with exactly one corrective action.
#[cfg(feature = "hybrid-local")]
fn probe_fastembed_offline(model: &str) -> anyhow::Result<()> {
    use crate::config::Config;
    use crate::hybrid::models;
    use crate::hybrid::runtime;

    let global = Config::global()?;
    runtime::resolve(&global.providers)?;
    let entry = models::entry(model)?;
    if !models::cached(entry)? {
        return Err(Error::Admission {
            message: format!(
                "model \"{model}\" is not fetched; run `docsbase models fetch {model}`"
            ),
        }
        .into());
    }
    Ok(())
}

#[cfg(not(feature = "hybrid-local"))]
fn probe_fastembed_offline(_model: &str) -> anyhow::Result<()> {
    Err(Error::Admission {
        message: "fastembed needs the hybrid-local build; rebuild with --features hybrid-local"
            .to_owned(),
    }
    .into())
}

/// Single-shot `OpenRouter` probe (FR-8): one embed of `docsbase probe`, one
/// rerank of (`docsbase`, `probe`) — each exactly one POST (the fixture
/// payloads below are asserted by the fake-transport tests), no retry
/// wrapper. 401/403 → `Admission` (`check key`); transport failures →
/// `Admission` (retry).
///
/// # Errors
/// Admission for missing/rejected key or transport failure; `Internal` for
/// an incompatible response schema.
#[cfg(feature = "hybrid-remote")]
fn probe_openrouter_singleshot(spec: &ProbeSpec) -> anyhow::Result<()> {
    use crate::hybrid::openrouter::ReqwestTransport;
    let transport = ReqwestTransport::new()?;
    probe_openrouter_singleshot_with(&transport, spec)
}

#[cfg(not(feature = "hybrid-remote"))]
fn probe_openrouter_singleshot(_spec: &ProbeSpec) -> anyhow::Result<()> {
    Err(Error::Admission {
        message: "OpenRouter needs the hybrid-remote build; rebuild with --features hybrid-remote"
            .to_owned(),
    }
    .into())
}

/// Probe core over an injected transport (test seam).
#[cfg(feature = "hybrid-remote")]
fn probe_openrouter_singleshot_with(
    transport: &dyn crate::hybrid::wire::Transport,
    spec: &ProbeSpec,
) -> anyhow::Result<()> {
    if let Some(dims) = spec.dims {
        probe_openrouter_embed(transport, &spec.model, dims)?;
    }
    if spec.window.is_some() {
        probe_openrouter_rerank(transport, &spec.model)?;
    }
    Ok(())
}

/// One embed POST, single-shot (no retry wrapper).
#[cfg(feature = "hybrid-remote")]
fn probe_openrouter_embed(
    transport: &dyn crate::hybrid::wire::Transport,
    model: &str,
    dims: u32,
) -> anyhow::Result<()> {
    use crate::hybrid::openrouter::BASE_URL;

    let url = format!("{BASE_URL}/embeddings");
    let request = serde_json::json!({
        "model": model,
        "input": ["docsbase probe"],
        "dimensions": dims,
        "input_type": "document",
    });
    let body = serde_json::to_string(&request)
        .map_err(|err| Error::internal_with_source("encode probe embed", err))?;
    let response = transport
        .post_json(&url, &bearer_headers()?, &body)
        .map_err(|err| Error::Admission {
            message: format!(
                "probe embed failed: {}; check the network and retry",
                err.inner_message()
            ),
        })?;
    classify_probe_status(response.status, "embed")?;
    let parsed: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|err| Error::internal_with_source("incompatible OpenRouter probe schema", err))?;
    let width_ok = parsed["data"]
        .get(0)
        .and_then(|datum| datum.get("embedding"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|values| values.len() == dims as usize);
    if !width_ok {
        return Err(Error::internal(format!(
            "probe embed returned a wrong shape for width {dims}"
        ))
        .into());
    }
    Ok(())
}

/// One rerank POST, single-shot (no retry wrapper).
#[cfg(feature = "hybrid-remote")]
fn probe_openrouter_rerank(
    transport: &dyn crate::hybrid::wire::Transport,
    model: &str,
) -> anyhow::Result<()> {
    use crate::hybrid::openrouter::BASE_URL;

    let url = format!("{BASE_URL}/rerank");
    let request = serde_json::json!({
        "model": model,
        "query": "docsbase",
        "documents": ["probe"],
        "top_n": 2,
    });
    let body = serde_json::to_string(&request)
        .map_err(|err| Error::internal_with_source("encode probe rerank", err))?;
    let response = transport
        .post_json(&url, &bearer_headers()?, &body)
        .map_err(|err| Error::Admission {
            message: format!(
                "probe rerank failed: {}; check the network and retry",
                err.inner_message()
            ),
        })?;
    classify_probe_status(response.status, "rerank")?;
    let parsed: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|err| Error::internal_with_source("incompatible OpenRouter probe schema", err))?;
    let count = parsed["results"].as_array().map_or(0, Vec::len);
    if count != 2 {
        return Err(Error::internal(format!(
            "probe rerank returned {count} scores for two documents"
        ))
        .into());
    }
    Ok(())
}

/// Maps probe HTTP statuses to admission errors (single-shot: no retry).
#[cfg(feature = "hybrid-remote")]
fn classify_probe_status(status: u16, stage: &str) -> anyhow::Result<()> {
    match status {
        200 => Ok(()),
        401 | 403 => Err(Error::Admission {
            message: format!(
                "probe {stage} was rejected (HTTP {status}); check OPENROUTER_API_KEY"
            ),
        }
        .into()),
        other => Err(Error::Admission {
            message: format!("probe {stage} returned HTTP {other}; check the network and retry"),
        }
        .into()),
    }
}

/// Bearer headers for `OpenRouter` probes; the key is never stored or logged
/// (FR-3).
#[cfg(feature = "hybrid-remote")]
fn bearer_headers() -> crate::error::Result<Vec<(String, String)>> {
    use crate::hybrid::openrouter::X_TITLE;
    let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::Admission {
        message: "OPENROUTER_API_KEY is not set; export it to use OpenRouter".to_owned(),
    })?;
    if key.trim().is_empty() {
        return Err(Error::Admission {
            message: "OPENROUTER_API_KEY is empty".to_owned(),
        });
    }
    Ok(vec![
        ("Authorization".to_owned(), format!("Bearer {key}")),
        ("X-Title".to_owned(), X_TITLE.to_owned()),
    ])
}

/// Reads the global config file; a missing file means "no global overrides
/// yet" (same semantics as the project reader).
fn read_global_toml() -> anyhow::Result<Option<toml::Value>> {
    let path = global_config_file()?;
    if path.exists() {
        read_project_toml(&path)
    } else {
        Ok(None)
    }
}

/// Global config file (`DOCSBASE_CONFIG_DIR` override honored).
///
/// # Errors
/// Returns [`Error::Internal`] when the config directory cannot be determined.
fn global_config_file() -> anyhow::Result<PathBuf> {
    Ok(crate::config::paths::config_dir()?.join("config.toml"))
}

/// Writes the merged table into the global config, preserving every other
/// section including `[providers]` (FR-1: providers live only in global).
/// Creates the config directory on first use.
#[cfg(test)]
fn write_global_toml(existing: Option<&toml::Value>, table: &toml::Value) -> anyhow::Result<()> {
    let path = global_config_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::internal_with_source(format!("create {}", parent.display()), err)
        })?;
    }
    // Cross-process serialization with `connect`'s provider writes (C13).
    crate::config::paths::with_config_write_lock(&path, || {
        write_project_toml(&path, existing, table)
    })
}

/// Merges the resolved `hybrid` table into a config file, preserving
/// every other key. Comments are not preserved by the TOML round-trip.
/// The write is atomic: readers see the old or the new file, never a
/// half-written one (same temp + rename pattern as the daemon state).
fn write_project_toml(
    path: &std::path::Path,
    existing: Option<&toml::Value>,
    table: &toml::Value,
) -> anyhow::Result<()> {
    let hybrid = table
        .get("hybrid")
        .ok_or_else(|| Error::internal("resolved config has no hybrid table; refusing to write"))?;
    let mut root = existing
        .cloned()
        .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
    let map = root.as_table_mut().ok_or_else(|| {
        Error::internal(format!(
            "existing {} is not a TOML table; refusing to overwrite",
            path.display()
        ))
    })?;
    map.insert("hybrid".to_owned(), hybrid.clone());
    let rendered =
        toml::to_string(&root).map_err(|err| Error::internal_with_source("render config", err))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, rendered)
        .map_err(|err| Error::internal_with_source(format!("write {}", tmp.display()), err))?;
    std::fs::rename(&tmp, path)
        .map_err(|err| Error::internal_with_source(format!("publish {}", path.display()), err))?;
    Ok(())
}

/// Prints pins, storage estimate and the reload note.
fn print_summary(table: &toml::Value) {
    use crate::hybrid::EmbeddingCfg;
    use crate::hybrid::config::embedding_model_id;

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
        let provider = stage
            .get("provider")
            .and_then(toml::Value::as_str)
            .unwrap_or(PROVIDER_OPENROUTER);
        let pin = if cfg!(feature = "hybrid-local") {
            embedding_model_id(&EmbeddingCfg {
                provider: provider.to_owned(),
                model: model.to_owned(),
                dimensions: u32::try_from(dims).unwrap_or_default(),
                normalize: true,
            })
        } else {
            format!("{provider}/{model}")
        };
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
        let provider = stage
            .get("provider")
            .and_then(toml::Value::as_str)
            .unwrap_or(PROVIDER_OPENROUTER);
        println!("rerank pin: {provider}/{model}");
    }
    println!("comments in .docsbase.toml are not preserved by this rewrite;");
    println!("applies on the next project open (restart a running detached daemon).");
}
#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    #[cfg(feature = "hybrid-remote")]
    use std::sync::Mutex;

    use super::*;
    #[cfg(feature = "hybrid-remote")]
    use crate::hybrid::wire::{Transport, TransportResponse};

    use crate::cli::test_env::ENV_LOCK;

    #[allow(unsafe_code)] // env mutation is confined to this locked section
    fn with_env<K: AsRef<OsStr>, V: AsRef<OsStr>>(key: K, value: V, f: impl FnOnce()) {
        with_envs(&[(key, value)], f);
    }

    /// Like [`with_env`] but sets several variables atomically under the
    /// shared env lock (nested `with_env` calls would deadlock).
    #[allow(unsafe_code)] // env mutation is confined to this locked section
    fn with_envs<K: AsRef<OsStr>, V: AsRef<OsStr>>(pairs: &[(K, V)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().expect("env lock");
        // std::env::{set_var,remove_var} are unsafe in edition 2024.
        // SAFETY: env vars are only mutated inside this lock, and the test
        // process runs a single thread at this point (no concurrent access).
        let prev: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = pairs
            .iter()
            .map(|(key, _)| (key.as_ref().to_os_string(), std::env::var_os(key.as_ref())))
            .collect();
        unsafe {
            for (key, value) in pairs {
                std::env::set_var(key.as_ref(), value);
            }
        }
        f();
        // SAFETY: same single-threaded critical section as above.
        unsafe {
            for (key, old) in prev {
                match old {
                    Some(value) => std::env::set_var(&key, value),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }

    /// Like [`with_env`] but guarantees the variable is absent for `f`
    /// (the missing-key probe must test the truly-unset case).
    #[cfg_attr(not(feature = "hybrid-remote"), allow(dead_code))]
    #[allow(unsafe_code)]
    fn without_env<K: AsRef<OsStr>>(key: K, f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let prev = std::env::var_os(key.as_ref());
        unsafe {
            std::env::remove_var(key.as_ref());
        }
        f();
        // SAFETY: same single-threaded critical section as above.
        unsafe {
            if let Some(v) = prev {
                std::env::set_var(key.as_ref(), v);
            }
        }
    }

    fn parse(toml_text: &str) -> toml::Value {
        toml::from_str(toml_text).expect("test TOML parses")
    }

    fn embed_args(model: &str, provider: Option<&str>, dims: Option<u32>) -> EmbedArgs {
        EmbedArgs {
            model: model.to_owned(),
            provider: provider.map(str::to_owned),
            dims,
        }
    }

    #[cfg_attr(not(feature = "hybrid-local"), allow(dead_code))]
    fn rerank_args(model: &str, provider: Option<&str>, window: Option<u32>) -> RerankArgs {
        RerankArgs {
            model: model.to_owned(),
            provider: provider.map(str::to_owned),
            window,
        }
    }

    fn use_args(purpose: UsePurpose) -> UseArgs {
        UseArgs {
            purpose,
            dry_run: true,
            path: None,
            global: false,
            local: false,
        }
    }

    fn embedding_stage(
        provider: &str,
        model: &str,
        dims: u32,
    ) -> toml::map::Map<String, toml::Value> {
        let mut stage = toml::map::Map::new();
        stage.insert(
            "provider".to_owned(),
            toml::Value::String(provider.to_owned()),
        );
        stage.insert("model".to_owned(), toml::Value::String(model.to_owned()));
        stage.insert(
            "dimensions".to_owned(),
            toml::Value::Integer(i64::from(dims)),
        );
        stage.insert("normalize".to_owned(), toml::Value::Boolean(true));
        stage
    }

    fn rerank_stage(
        provider: &str,
        model: &str,
        window: u32,
    ) -> toml::map::Map<String, toml::Value> {
        let mut stage = toml::map::Map::new();
        stage.insert(
            "provider".to_owned(),
            toml::Value::String(provider.to_owned()),
        );
        stage.insert("model".to_owned(), toml::Value::String(model.to_owned()));
        stage.insert("window".to_owned(), toml::Value::Integer(i64::from(window)));
        stage
    }

    // ---------- scope matrix (FR-8, SC-5) ----------

    #[test]
    fn scope_both_flags_error() {
        let mut args = use_args(UsePurpose::Embed(embed_args("m", None, None)));
        args.local = true;
        args.global = true;
        let err = resolve_scope(&args).expect_err("both flags");
        assert!(err.to_string().contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn scope_path_with_global_error() {
        let mut args = use_args(UsePurpose::Embed(embed_args("m", None, None)));
        args.path = Some(PathBuf::from("/tmp"));
        args.global = true;
        let err = resolve_scope(&args).expect_err("path+global");
        assert!(err.to_string().contains("--path"), "{err}");
    }

    #[test]
    fn scope_global_wins() {
        let mut args = use_args(UsePurpose::Embed(embed_args("m", None, None)));
        args.global = true;
        assert_eq!(resolve_scope(&args).expect("global"), Scope::Global);
    }

    #[test]
    fn scope_inside_project_default_and_local() {
        let dir = tempfile::TempDir::new().expect("tmp");
        std::fs::write(dir.path().join(".docsbase.toml"), "[hybrid]\n").expect("project file");
        let mut args = use_args(UsePurpose::Embed(embed_args("m", None, None)));
        args.path = Some(dir.path().to_path_buf());
        assert_eq!(
            resolve_scope(&args).expect("default"),
            Scope::Project(dir.path().to_path_buf())
        );
        args.local = true;
        assert_eq!(
            resolve_scope(&args).expect("local"),
            Scope::Project(dir.path().to_path_buf())
        );
    }

    #[test]
    fn scope_outside_project_requires_global() {
        let dir = tempfile::TempDir::new().expect("tmp"); // no .docsbase.toml
        let mut args = use_args(UsePurpose::Embed(embed_args("m", None, None)));
        args.path = Some(dir.path().to_path_buf());
        let err = resolve_scope(&args).expect_err("outside project");
        assert!(err.to_string().contains("--global"), "{err}");
    }

    // ---------- purpose / provider inference (FR-8) ----------

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn registry_name_implies_fastembed_and_infers_dims() {
        let (provider, dims) =
            resolve_embed_stage(&embed_args("bge-small-en-v1.5", None, None)).expect("embed");
        assert_eq!(provider, PROVIDER_FASTEMBED);
        assert_eq!(dims, 384);
        let err = resolve_embed_stage(&embed_args("bge-small-en-v1.5", None, Some(123)))
            .expect_err("dims mismatch");
        assert!(err.to_string().contains("registry"), "{err}");
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn rerank_purpose_rejects_embedding_row() {
        let err = resolve_rerank_stage(&rerank_args("bge-small-en-v1.5", None, None))
            .expect_err("wrong task");
        assert!(err.to_string().contains("not for rerank"), "{err}");
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn embed_purpose_rejects_rerank_row() {
        let err = resolve_embed_stage(&embed_args("jina-reranker-v1-turbo-en", None, None))
            .expect_err("wrong task");
        assert!(err.to_string().contains("not for embedding"), "{err}");
    }

    #[test]
    fn unknown_model_requires_provider() {
        let err = resolve_embed_stage(&embed_args("some/model", None, Some(128)))
            .expect_err("unknown without provider");
        assert!(err.to_string().contains("--provider"), "{err}");
        let (provider, _) =
            resolve_embed_stage(&embed_args("some/model", Some("openrouter"), Some(128)))
                .expect("explicit provider");
        assert_eq!(provider, PROVIDER_OPENROUTER);
    }

    #[test]
    fn openrouter_requires_dims() {
        let err = resolve_embed_stage(&embed_args("some/model", Some("openrouter"), None))
            .expect_err("missing dims");
        assert!(err.to_string().contains("--dims"), "{err}");
        let err = resolve_embed_stage(&embed_args("some/model", Some("openrouter"), Some(0)))
            .expect_err("zero dims");
        assert!(err.to_string().contains("positive"), "{err}");
    }

    #[test]
    fn window_default_and_bounds() {
        assert_eq!(
            resolve_window(None).expect("default"),
            DEFAULT_RERANK_WINDOW
        );
        assert_eq!(resolve_window(Some(20)).expect("ok"), 20);
        let err = resolve_window(Some(5)).expect_err("below min");
        assert!(err.to_string().contains("--window"), "{err}");
    }

    // ---------- stage merge / validation ----------

    #[test]
    fn fresh_file_gets_enabled_table() {
        let table = merge_stage(None, "embedding", embedding_stage("fastembed", "bge", 384))
            .expect("merge");
        let hybrid = table.get("hybrid").expect("hybrid table");
        assert_eq!(
            hybrid.get("enabled").and_then(toml::Value::as_bool),
            Some(true)
        );
        let embedding = hybrid.get("embedding").expect("embedding");
        assert_eq!(
            embedding.get("model").and_then(toml::Value::as_str),
            Some("bge")
        );
        assert_eq!(
            embedding
                .get("dimensions")
                .and_then(toml::Value::as_integer),
            Some(384)
        );
    }

    #[test]
    fn untouched_stages_and_ranking_survive() {
        let existing = parse(
            "max_file_size = 1\n[hybrid]\nranking = \"rrf\"\n[hybrid.reranker]\nprovider = \"openrouter\"\nmodel = \"old/r\"\nwindow = 20\n",
        );
        let table = merge_stage(
            Some(&existing),
            "embedding",
            embedding_stage("openrouter", "new/e", 256),
        )
        .expect("merge");
        let hybrid = table.get("hybrid").expect("hybrid table");
        assert_eq!(
            hybrid.get("ranking").and_then(toml::Value::as_str),
            Some("rrf")
        );
        let reranker = hybrid.get("reranker").expect("reranker kept");
        assert_eq!(
            reranker.get("model").and_then(toml::Value::as_str),
            Some("old/r")
        );
        assert_eq!(
            reranker.get("window").and_then(toml::Value::as_integer),
            Some(20)
        );
    }

    #[test]
    fn selected_stage_only_is_written() {
        let existing = parse(
            "[hybrid]\n[hybrid.embedding]\nprovider = \"openrouter\"\nmodel = \"old/e\"\ndimensions = 128\nnormalize = true\n",
        );
        let table = merge_stage(
            Some(&existing),
            "reranker",
            rerank_stage("openrouter", "new/r", 30),
        )
        .expect("merge");
        let hybrid = table.get("hybrid").expect("hybrid table");
        let embedding = hybrid.get("embedding").expect("embedding untouched");
        assert_eq!(
            embedding.get("model").and_then(toml::Value::as_str),
            Some("old/e")
        );
        assert_eq!(
            embedding
                .get("dimensions")
                .and_then(toml::Value::as_integer),
            Some(128)
        );
        let reranker = hybrid.get("reranker").expect("reranker set");
        assert_eq!(
            reranker.get("window").and_then(toml::Value::as_integer),
            Some(30)
        );
    }

    #[test]
    fn preserved_stages_validated() {
        let zero_dims = parse(
            "[hybrid]\n[hybrid.embedding]\nprovider = \"openrouter\"\nmodel = \"m/e\"\ndimensions = 0\n",
        );
        let err = merge_stage(
            Some(&zero_dims),
            "reranker",
            rerank_stage("openrouter", "m/r", 20),
        )
        .expect_err("stale preserved dims");
        assert!(err.to_string().contains("models use embed"), "{err}");

        let bogus_ranking = parse("[hybrid]\nranking = \"bogus\"\n");
        let err = merge_stage(
            Some(&bogus_ranking),
            "embedding",
            embedding_stage("openrouter", "m/e", 128),
        )
        .expect_err("unknown preserved ranking");
        assert!(err.to_string().contains("ranking"), "{err}");
    }

    #[test]
    fn free_routes_get_a_privacy_note() {
        let free =
            parse("[hybrid]\n[hybrid.embedding]\nmodel = \"nvidia/nemotron-3-embed-1b:free\"\n");
        let note = free_route_note(&free).expect("free route noted");
        assert!(note.contains("train on prompts"), "{note}");
        let paid = parse("[hybrid]\n[hybrid.embedding]\nmodel = \"qwen/qwen3-embedding-8b\"\n");
        assert!(free_route_note(&paid).is_none(), "paid route stays silent");
        assert!(free_route_note(&parse("[other]\n")).is_none());
    }

    // ---------- writes ----------

    #[test]
    fn project_write_roundtrip_keeps_unrelated_keys_and_other_stage() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let path = dir.path().join(".docsbase.toml");
        std::fs::write(
            &path,
            "max_file_size = 1\n[hybrid]\nranking = \"rrf\"\n[hybrid.reranker]\nprovider = \"openrouter\"\nmodel = \"old/r\"\nwindow = 20\n",
        )
        .expect("seed");
        let existing = read_project_toml(&path).expect("read").expect("some");
        // Default project scope: write only the embedding stage.
        let table = merge_stage(
            Some(&existing),
            "embedding",
            embedding_stage("openrouter", "new/e", 256),
        )
        .expect("merge");
        write_project_toml(&path, Some(&existing), &table).expect("write");
        let reloaded: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).expect("read back")).expect("reparse");
        assert_eq!(
            reloaded
                .get("max_file_size")
                .and_then(toml::Value::as_integer),
            Some(1),
            "unrelated keys survive"
        );
        let hybrid = reloaded.get("hybrid").expect("hybrid table");
        assert_eq!(
            hybrid.get("ranking").and_then(toml::Value::as_str),
            Some("rrf")
        );
        let embedding = hybrid.get("embedding").expect("embedding written");
        assert_eq!(
            embedding.get("model").and_then(toml::Value::as_str),
            Some("new/e")
        );
        assert_eq!(
            embedding
                .get("dimensions")
                .and_then(toml::Value::as_integer),
            Some(256)
        );
        let reranker = hybrid.get("reranker").expect("reranker untouched");
        assert_eq!(
            reranker.get("model").and_then(toml::Value::as_str),
            Some("old/r")
        );
        assert_eq!(
            reranker.get("window").and_then(toml::Value::as_integer),
            Some(20)
        );
    }

    #[test]
    fn concurrent_global_writers_both_survive() {
        // Round-2 P1 regression: a slow writer holding the config lock must
        // not cause a second writer to drop its stage. Writer A takes its
        // snapshot, writes `embedding`, then deliberately holds the lock;
        // writer B starts after A's snapshot (so its own stale snapshot
        // misses `embedding`) and must re-read under the lock — the final
        // file keeps both stages. (Old code: B wrote from its pre-lock
        // snapshot and erased A's stage.)
        use std::time::Duration;
        let dir = tempfile::TempDir::new().expect("tmp");
        let path = dir.path().join("config.toml");
        let path_a = path.clone();
        let writer_a = std::thread::spawn(move || {
            crate::config::paths::with_config_write_lock(&path_a, || {
                let current = read_config_table(&path_a)?;
                let table = merge_stage(
                    current.as_ref(),
                    "embedding",
                    embedding_stage("fastembed", "bge", 384),
                )?;
                write_project_toml(&path_a, current.as_ref(), &table)?;
                // Hold the lock so B (below) must wait and re-read.
                std::thread::sleep(Duration::from_millis(400));
                Ok(())
            })
            .expect("writer A");
        });
        // Start B after A's snapshot, while A holds the lock.
        std::thread::sleep(Duration::from_millis(50));
        apply_stage_locked(&path, "reranker", rerank_stage("fastembed", "jina", 50))
            .expect("writer B");
        writer_a.join().expect("join A");
        let reloaded = parse(&std::fs::read_to_string(&path).expect("read back"));
        let hybrid = reloaded.get("hybrid").expect("hybrid table");
        assert!(
            hybrid.get("embedding").is_some(),
            "embedding stage survives: {reloaded:?}"
        );
        assert!(
            hybrid.get("reranker").is_some(),
            "reranker stage survives: {reloaded:?}"
        );
    }

    #[test]
    fn global_write_preserves_providers_and_unrelated_sections() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            "max_file_size = 1\n[providers.fastembed]\nenabled = true\n",
        )
        .expect("seed");
        let existing = read_project_toml(&config).expect("read").expect("some");
        let table = merge_stage(None, "embedding", embedding_stage("fastembed", "bge", 384))
            .expect("merge");
        with_env("DOCSBASE_CONFIG_DIR", dir.path(), || {
            write_global_toml(Some(&existing), &table).expect("write");
            let reloaded = parse(&std::fs::read_to_string(&config).expect("read back"));
            assert_eq!(
                reloaded
                    .get("max_file_size")
                    .and_then(toml::Value::as_integer),
                Some(1),
                "unrelated sections survive"
            );
            assert!(reloaded.get("providers").is_some(), "[providers] survives");
            let providers = reloaded.get("providers").expect("providers");
            assert_eq!(
                providers
                    .get("fastembed")
                    .and_then(|entry| entry.get("enabled"))
                    .and_then(toml::Value::as_bool),
                Some(true)
            );
            let hybrid = reloaded.get("hybrid").expect("hybrid table");
            assert_eq!(
                hybrid
                    .get("embedding")
                    .and_then(|embedding| embedding.get("dimensions"))
                    .and_then(toml::Value::as_integer),
                Some(384)
            );
            assert!(hybrid.get("reranker").is_none(), "other stage untouched");
        });
    }

    #[test]
    fn global_write_creates_config_dir() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let config_dir = dir.path().join("nested").join("config");
        let table =
            merge_stage(None, "reranker", rerank_stage("fastembed", "jina", 50)).expect("merge");
        with_env("DOCSBASE_CONFIG_DIR", &config_dir, || {
            write_global_toml(None, &table).expect("write");
            assert!(config_dir.join("config.toml").exists(), "created");
        });
    }

    // ---------- fastembed offline probe (FR-8, SC-5) ----------

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn fastembed_probe_missing_runtime_gives_connect_hint() {
        let dir = tempfile::TempDir::new().expect("tmp");
        with_env("DOCSBASE_CONFIG_DIR", dir.path(), || {
            let err = probe_fastembed_offline("bge-small-en-v1.5").expect_err("no runtime");
            assert!(err.to_string().contains("connect fastembed"), "{err}");
        });
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn fastembed_probe_uncached_model_gives_fetch_hint() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let runtime = dir.path().join("libonnxruntime.so");
        std::fs::write(&runtime, b"fake").expect("touch");
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "[providers.fastembed]\nenabled = true\nruntime_path = \"{}\"\n",
                runtime.display()
            ),
        )
        .expect("seed");
        // Isolate both the config dir AND the model cache: the real user
        // cache may already contain a fetched copy (live T8 runs).
        let cache = tempfile::TempDir::new().expect("tmp");
        with_envs(
            &[
                ("DOCSBASE_CONFIG_DIR", dir.path()),
                ("DOCSBASE_CACHE_DIR", cache.path()),
            ],
            || {
                let err = probe_fastembed_offline("bge-small-en-v1.5").expect_err("not fetched");
                assert!(err.to_string().contains("models fetch"), "{err}");
            },
        );
    }

    // ---------- OpenRouter single-shot probe (FR-8, SC-5) ----------

    #[cfg(feature = "hybrid-remote")]
    struct ProbeFake {
        embed_status: u16,
        embed_body: String,
        rerank_status: u16,
        rerank_body: String,
        fail_transport: bool,
        calls: Mutex<Vec<(String, String)>>,
    }

    #[cfg(feature = "hybrid-remote")]
    impl Transport for ProbeFake {
        fn post_json(
            &self,
            url: &str,
            _headers: &[(String, String)],
            body: &str,
        ) -> crate::error::Result<TransportResponse> {
            self.calls
                .lock()
                .expect("calls")
                .push((url.to_owned(), body.to_owned()));
            if self.fail_transport {
                return Err(Error::internal("connection reset"));
            }
            let (status, body) = if url.contains("/embeddings") {
                (self.embed_status, &self.embed_body)
            } else {
                (self.rerank_status, &self.rerank_body)
            };
            Ok(TransportResponse {
                status,
                body: body.clone(),
                retry_after: None,
            })
        }

        fn get_json(
            &self,
            _url: &str,
            _headers: &[(String, String)],
        ) -> crate::error::Result<TransportResponse> {
            Err(Error::internal("GET not used by the probe"))
        }
    }

    #[cfg(feature = "hybrid-remote")]
    fn embed_ok_body(dims: u32) -> String {
        serde_json::json!({ "data": [{ "embedding": vec![0.0f32; dims as usize] }] }).to_string()
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    fn openrouter_probe_embed_ok() {
        let fake = ProbeFake {
            embed_status: 200,
            embed_body: embed_ok_body(128),
            rerank_status: 200,
            rerank_body: String::new(),
            fail_transport: false,
            calls: Mutex::new(Vec::new()),
        };
        with_env("OPENROUTER_API_KEY", "k", || {
            let spec = ProbeSpec {
                provider: PROVIDER_OPENROUTER.to_owned(),
                model: "m/e".to_owned(),
                dims: Some(128),
                window: None,
            };
            probe_openrouter_singleshot_with(&fake, &spec).expect("probe ok");
            let calls = fake.calls.lock().expect("calls");
            assert_eq!(calls.len(), 1, "one embed call");
            let (url, body) = &calls[0];
            assert!(url.contains("/embeddings"), "{url}");
            let payload: serde_json::Value = serde_json::from_str(body).expect("json");
            assert_eq!(payload["model"], "m/e");
            assert_eq!(payload["input"][0], "docsbase probe");
            assert_eq!(payload["dimensions"], 128);
        });
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    fn openrouter_probe_both_stages_single_shot_each() {
        let fake = ProbeFake {
            embed_status: 200,
            embed_body: embed_ok_body(128),
            rerank_status: 200,
            rerank_body: serde_json::json!({
                "results": [
                    { "index": 0, "relevance_score": 0.9 },
                    { "index": 1, "relevance_score": 0.1 },
                ]
            })
            .to_string(),
            fail_transport: false,
            calls: Mutex::new(Vec::new()),
        };
        with_env("OPENROUTER_API_KEY", "k", || {
            let spec = ProbeSpec {
                provider: PROVIDER_OPENROUTER.to_owned(),
                model: "m/e".to_owned(),
                dims: Some(128),
                window: Some(50),
            };
            probe_openrouter_singleshot_with(&fake, &spec).expect("probe ok");
        });
        let calls = fake.calls.lock().expect("calls");
        assert_eq!(calls.len(), 2, "one embed + one rerank");
        let (embed_url, embed_body) = &calls[0];
        assert!(embed_url.contains("/embeddings"), "{embed_url}");
        let embed_payload: serde_json::Value = serde_json::from_str(embed_body).expect("json");
        assert_eq!(embed_payload["model"], "m/e");
        assert_eq!(embed_payload["input"][0], "docsbase probe");
        assert_eq!(embed_payload["dimensions"], 128);
        let (rerank_url, rerank_body) = &calls[1];
        assert!(rerank_url.contains("/rerank"), "{rerank_url}");
        let rerank_payload: serde_json::Value = serde_json::from_str(rerank_body).expect("json");
        assert_eq!(rerank_payload["model"], "m/e");
        assert_eq!(rerank_payload["query"], "docsbase");
        assert_eq!(rerank_payload["documents"][0], "probe");
        assert_eq!(rerank_payload["top_n"], 2);
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    fn openrouter_probe_401_is_admission_check_key() {
        let fake = ProbeFake {
            embed_status: 401,
            embed_body: "{}".to_owned(),
            rerank_status: 200,
            rerank_body: String::new(),
            fail_transport: false,
            calls: Mutex::new(Vec::new()),
        };
        with_env("OPENROUTER_API_KEY", "k", || {
            let spec = ProbeSpec {
                provider: PROVIDER_OPENROUTER.to_owned(),
                model: "m/e".to_owned(),
                dims: Some(128),
                window: None,
            };
            let err = probe_openrouter_singleshot_with(&fake, &spec).expect_err("401");
            assert!(err.to_string().contains("OPENROUTER_API_KEY"), "{err}");
        });
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    fn openrouter_probe_transport_error_is_admission_retry() {
        let fake = ProbeFake {
            embed_status: 0,
            embed_body: String::new(),
            rerank_status: 0,
            rerank_body: String::new(),
            fail_transport: true,
            calls: Mutex::new(Vec::new()),
        };
        with_env("OPENROUTER_API_KEY", "k", || {
            let spec = ProbeSpec {
                provider: PROVIDER_OPENROUTER.to_owned(),
                model: "m/e".to_owned(),
                dims: Some(128),
                window: None,
            };
            let err = probe_openrouter_singleshot_with(&fake, &spec).expect_err("transport");
            assert!(err.to_string().contains("retry"), "{err}");
        });
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    fn openrouter_probe_wrong_width_is_internal() {
        let fake = ProbeFake {
            embed_status: 200,
            embed_body: embed_ok_body(64), // width 64, spec says 128
            rerank_status: 200,
            rerank_body: String::new(),
            fail_transport: false,
            calls: Mutex::new(Vec::new()),
        };
        with_env("OPENROUTER_API_KEY", "k", || {
            let spec = ProbeSpec {
                provider: PROVIDER_OPENROUTER.to_owned(),
                model: "m/e".to_owned(),
                dims: Some(128),
                window: None,
            };
            probe_openrouter_singleshot_with(&fake, &spec).expect_err("width mismatch");
        });
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    fn openrouter_probe_missing_key_is_admission() {
        let fake = ProbeFake {
            embed_status: 200,
            embed_body: embed_ok_body(128),
            rerank_status: 200,
            rerank_body: String::new(),
            fail_transport: false,
            calls: Mutex::new(Vec::new()),
        };
        // No OPENROUTER_API_KEY present (removed, not emptied).
        without_env("OPENROUTER_API_KEY", || {
            let spec = ProbeSpec {
                provider: PROVIDER_OPENROUTER.to_owned(),
                model: "m/e".to_owned(),
                dims: Some(128),
                window: None,
            };
            let err = probe_openrouter_singleshot_with(&fake, &spec).expect_err("no key");
            assert!(err.to_string().contains("OPENROUTER_API_KEY"), "{err}");
        });
    }

    #[test]
    fn repin_embedding_note_on_change_and_first_pin() {
        let existing = parse(
            "[hybrid]\n[hybrid.embedding]\nprovider = \"openrouter\"\nmodel = \"m/e\"\ndimensions = 128\nnormalize = true\n",
        );
        let changed = merge_stage(
            Some(&existing),
            "embedding",
            embedding_stage("openrouter", "m/e", 256),
        )
        .expect("merge");
        let note = repin_embedding_note(Some(&existing), &changed).expect("pin changed");
        assert!(note.contains("re-embedded"), "{note}");
        assert!(note.contains("ANN"), "{note}");

        let same = merge_stage(
            Some(&existing),
            "embedding",
            embedding_stage("openrouter", "m/e", 128),
        )
        .expect("merge");
        assert!(
            repin_embedding_note(Some(&existing), &same).is_none(),
            "same pin silent"
        );

        let first = merge_stage(None, "embedding", embedding_stage("openrouter", "m/e", 128))
            .expect("merge");
        let note = repin_embedding_note(None, &first).expect("first pin");
        assert!(note.contains("backfill"), "{note}");
    }

    // ---------- models remove (FR-7) ----------

    #[cfg(feature = "hybrid-local")]
    fn remove_args(name: &str, force: bool) -> RemoveArgs {
        RemoveArgs {
            name: name.to_owned(),
            force,
        }
    }

    #[cfg(feature = "hybrid-local")]
    fn seed_model_cache(root: &Path, name: &str) -> PathBuf {
        // Use the registry's real pinned revision (remove keys on it).
        let revision = crate::hybrid::models::entry(name)
            .expect("registry")
            .revision;
        let model_dir = root.join("models").join(name).join(revision);
        std::fs::create_dir_all(&model_dir).expect("create model dir");
        std::fs::write(model_dir.join("model.onnx"), b"fake").expect("file");
        model_dir
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_deletes_all_cached_revisions_including_stale() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let pinned = seed_model_cache(dir.path(), "bge-small-en-v1.5");
        // A stale revision from an older registry pin (must also go).
        let stale = pinned.parent().expect("parent").join("oldrev");
        std::fs::create_dir_all(&stale).expect("create stale rev");
        std::fs::write(stale.join("model.onnx"), b"x").expect("file");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            remove_cmd(&remove_args("bge-small-en-v1.5", false)).expect("remove");
        });
        assert!(!pinned.exists(), "pinned revision removed");
        assert!(!stale.exists(), "stale revision removed too");
        assert!(
            !pinned.parent().expect("parent").exists(),
            "model dir removed"
        );
    }
    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_stale_only_revisions_succeeds() {
        let dir = tempfile::TempDir::new().expect("tmp");
        // Registry bump: pinned revision absent, only a stale one remains.
        let stale = dir
            .path()
            .join("models")
            .join("bge-small-en-v1.5")
            .join("oldrev");
        std::fs::create_dir_all(&stale).expect("create stale rev");
        std::fs::write(stale.join("model.onnx"), b"x").expect("file");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            remove_cmd(&remove_args("bge-small-en-v1.5", false)).expect("remove stale-only");
        });
        assert!(!stale.exists(), "stale revision removed");
        assert!(
            !stale.parent().expect("parent").exists(),
            "model dir removed"
        );
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_unlocked_succeeds() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let model_dir = seed_model_cache(dir.path(), "bge-small-en-v1.5");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            remove_cmd(&remove_args("bge-small-en-v1.5", false)).expect("remove");
        });
        assert!(!model_dir.exists(), "model cache removed");
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_refuses_when_locked_then_succeeds_after_drop() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let model_dir = seed_model_cache(dir.path(), "bge-small-en-v1.5");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            // The exact shared guard T4/T5 instances hold for their lifetime.
            let guard = crate::hybrid::local::take_shared_lock(&model_dir).expect("shared lock");
            let err = remove_cmd(&remove_args("bge-small-en-v1.5", false))
                .expect_err("refused while loaded");
            assert!(err.to_string().contains("in use"), "{err}");
            drop(guard);
            remove_cmd(&remove_args("bge-small-en-v1.5", false)).expect("remove after drop");
        });
        assert!(!model_dir.exists(), "removed after guard drop");
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_force_when_locked() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let model_dir = seed_model_cache(dir.path(), "bge-small-en-v1.5");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            let _guard = crate::hybrid::local::take_shared_lock(&model_dir).expect("shared lock");
            remove_cmd(&remove_args("bge-small-en-v1.5", true)).expect("force remove");
        });
        assert!(!model_dir.exists(), "force removal deletes anyway");
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_refuses_when_fetch_inflight_on_staging() {
        // Round-2 D3: a first fetch installs into `.staging-*` while
        // holding the model-root lock exclusively; a concurrent remove must
        // be refused (never delete the in-flight download) even though no
        // revision dir exists yet.
        let dir = tempfile::TempDir::new().expect("tmp");
        let model_dir = dir.path().join("models").join("bge-small-en-v1.5");
        std::fs::create_dir_all(&model_dir).expect("create model dir");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            let lock_path = model_dir.join(".lock");
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .expect("lock open");
            let lock: &'static mut fd_lock::RwLock<std::fs::File> =
                Box::leak(Box::new(fd_lock::RwLock::new(file)));
            let _in_flight = lock.try_write().expect("fetch holds exclusive");
            let err = remove_cmd(&remove_args("bge-small-en-v1.5", false))
                .expect_err("refused while fetch in flight");
            assert!(err.to_string().contains("in use"), "{err}");
        });
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_refuses_when_stale_revision_locked() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let model_dir = seed_model_cache(dir.path(), "bge-small-en-v1.5");
        // A stale revision from an older registry pin, currently loaded.
        let stale = model_dir.parent().expect("parent").join("oldrev");
        std::fs::create_dir_all(&stale).expect("create stale rev");
        std::fs::write(stale.join("model.onnx"), b"x").expect("file");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            let guard = crate::hybrid::local::take_shared_lock(&stale).expect("shared lock");
            let err = remove_cmd(&remove_args("bge-small-en-v1.5", false))
                .expect_err("refused while stale revision loaded");
            assert!(err.to_string().contains("in use"), "{err}");
            drop(guard);
            remove_cmd(&remove_args("bge-small-en-v1.5", false)).expect("remove after drop");
        });
        assert!(!model_dir.exists(), "removed after guard drop");
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_force_when_stale_revision_locked() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let model_dir = seed_model_cache(dir.path(), "bge-small-en-v1.5");
        let stale = model_dir.parent().expect("parent").join("oldrev");
        std::fs::create_dir_all(&stale).expect("create stale rev");
        std::fs::write(stale.join("model.onnx"), b"x").expect("file");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            let _guard = crate::hybrid::local::take_shared_lock(&stale).expect("shared lock");
            remove_cmd(&remove_args("bge-small-en-v1.5", true)).expect("force remove");
        });
        assert!(
            !model_dir.exists(),
            "force removal deletes stale-locked rev anyway"
        );
    }

    #[cfg(feature = "hybrid-local")]
    #[test]
    fn remove_not_fetched_is_admission() {
        let dir = tempfile::TempDir::new().expect("tmp");
        with_env("DOCSBASE_CACHE_DIR", dir.path(), || {
            let err =
                remove_cmd(&remove_args("bge-small-en-v1.5", false)).expect_err("not fetched");
            assert!(err.to_string().contains("not fetched"), "{err}");
        });
    }
}
