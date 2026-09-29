//! Markdown discovery with ignore rules and root confinement (FR-14, FR-32; I4).

use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

use crate::config::Config;
use crate::error::{Error, Result};

/// Directories always skipped, regardless of git or custom ignore files.
pub const DEFAULT_IGNORED_DIRS: &[&str] =
    &["node_modules", "target", "vendor", "dist", "build", ".git"];

/// Custom ignore file consulted in addition to `.gitignore`.
pub const IGNORE_FILE: &str = ".docsbaseignore";

/// Walks `root` and yields every indexable `.md` file.
///
/// Skips hidden entries, built-in directories, `.gitignore` and
/// [`IGNORE_FILE`] matches. Ignore patterns that try to traverse outside the
/// root (`..`) are rejected up front (FR-32).
///
/// # Errors
/// Returns [`Error::Project`] for an escaping ignore pattern and
/// [`Error::Internal`] when the walker cannot be built.
pub fn walk(root: &Path, config: &Config) -> Result<impl Iterator<Item = Result<PathBuf>>> {
    validate_patterns(root, config)?;

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .follow_links(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .require_git(false)
        .add_custom_ignore_filename(IGNORE_FILE);

    if !config.ignores.is_empty() {
        let mut overrides = OverrideBuilder::new(root);
        for pattern in &config.ignores {
            let negative = format!("!{pattern}");
            overrides.add(&negative).map_err(|err| Error::Project {
                message: format!("invalid ignore pattern {pattern:?}: {err}"),
                instruction: None,
            })?;
        }
        let built = overrides.build().map_err(|err| Error::Project {
            message: format!("invalid ignore overrides: {err}"),
            instruction: None,
        })?;
        builder.overrides(built);
    }

    let root = root.to_path_buf();
    Ok(builder.build().filter_map(move |entry| match entry {
        Ok(entry) => {
            let path = entry.path();
            let is_file = entry.file_type().is_some_and(|kind| kind.is_file());
            if is_file && is_markdown(path) && !in_ignored_dir(&root, path) {
                Some(Ok(path.to_path_buf()))
            } else {
                None
            }
        }
        Err(err) => Some(Err(Error::internal_with_source(
            format!("walk: {err}"),
            err,
        ))),
    }))
}

/// Returns every file in `dir` (non-recursive) that [`walk`] would pick,
/// using the same ignore stack: hidden entries below `dir` (the directory
/// itself is assumed visible), `.gitignore` chains (including parent
/// directories and `.git/info/exclude`), `.docsbaseignore`, always-ignored
/// directories and configured patterns. Symlinks are never followed, so
/// results stay inside `root` (FR-32).
///
/// The watcher calls this once per directory of changed files instead of the
/// full recursive walk (FR-15).
///
/// # Errors
/// Returns [`Error::Internal`] when the walker cannot be built, and
/// [`Error::Project`] for escaping ignore patterns.
pub fn indexable_files(
    dir: &Path,
    root: &Path,
    config: &Config,
) -> Result<std::collections::HashSet<PathBuf>> {
    validate_patterns(root, config)?;

    let mut builder = WalkBuilder::new(dir);
    builder
        .hidden(true)
        .follow_links(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .require_git(false)
        .max_depth(Some(1))
        .add_custom_ignore_filename(IGNORE_FILE);

    if !config.ignores.is_empty() {
        let mut overrides = OverrideBuilder::new(root);
        for pattern in &config.ignores {
            let negative = format!("!{pattern}");
            overrides.add(&negative).map_err(|err| Error::Project {
                message: format!("invalid ignore pattern {pattern:?}: {err}"),
                instruction: None,
            })?;
        }
        let built = overrides.build().map_err(|err| Error::Project {
            message: format!("invalid ignore overrides: {err}"),
            instruction: None,
        })?;
        builder.overrides(built);
    }

    let mut files = std::collections::HashSet::new();
    for entry in builder.build() {
        let entry = entry.map_err(|err| {
            Error::internal_with_source(format!("walk {}: {err}", dir.display()), err)
        })?;
        let path = entry.path();
        let is_file = entry.file_type().is_some_and(|kind| kind.is_file());
        if is_file && is_markdown(path) && !in_ignored_dir(root, path) {
            files.insert(path.to_path_buf());
        }
    }
    Ok(files)
}

/// True when `path` is outside `root`, hidden or below an always-ignored
/// directory; used by the watcher before any filesystem access (FR-15).
#[must_use]
pub fn is_pruned(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    if in_ignored_dir(root, path) {
        return true;
    }
    relative.components().any(|component| {
        matches!(component, Component::Normal(name) if name.to_string_lossy().starts_with('.'))
    })
}

/// Canonicalizes `path` and guarantees it stays inside `root` (I4).
///
/// # Errors
/// Returns [`Error::Project`] when either path cannot be canonicalized or the
/// resolved candidate escapes the root (including via symlink).
pub fn resolve_in_root(root: &Path, path: &Path) -> Result<PathBuf> {
    let root = root.canonicalize().map_err(|err| Error::Project {
        message: format!("canonicalize root {}: {err}", root.display()),
        instruction: None,
    })?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let resolved = candidate.canonicalize().map_err(|err| Error::Project {
        message: format!("canonicalize {}: {err}", candidate.display()),
        instruction: None,
    })?;
    if !crate::platform::paths::is_under(&resolved, &root) {
        return Err(Error::Project {
            message: format!("{} escapes root {}", resolved.display(), root.display()),
            instruction: None,
        });
    }
    Ok(resolved)
}

pub(crate) fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

fn in_ignored_dir(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    relative.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        name.to_str()
            .is_some_and(|name| DEFAULT_IGNORED_DIRS.contains(&name))
    })
}

fn validate_patterns(root: &Path, config: &Config) -> Result<()> {
    for pattern in &config.ignores {
        reject_escape(root, pattern)?;
    }
    let ignore_file = root.join(IGNORE_FILE);
    match std::fs::read_to_string(&ignore_file) {
        Ok(text) => {
            for line in text.lines() {
                let pattern = line.trim();
                if pattern.is_empty() || pattern.starts_with('#') {
                    continue;
                }
                reject_escape(root, pattern)?;
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(Error::internal_with_source(
                format!("read {}: {err}", ignore_file.display()),
                err,
            ));
        }
    }
    Ok(())
}

fn reject_escape(root: &Path, pattern: &str) -> Result<()> {
    let bare = pattern.strip_prefix('!').unwrap_or(pattern);
    let escapes = bare.split(['/', '\\']).any(|component| component == "..");
    if escapes {
        return Err(Error::Project {
            message: format!(
                "ignore pattern {pattern:?} in {} must not contain '..'",
                root.display()
            ),
            instruction: Some("use root-relative patterns".to_owned()),
        });
    }
    Ok(())
}
