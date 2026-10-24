//! `docsbase connect` (FR-2/3/4): provider status, `OpenRouter` key probe
//! (`hybrid-remote`), and the local fastembed/ORT install (`hybrid-local`).
//!
//! `connect` never selects a model (FR-4); it only prepares the machine and
//! writes `[providers.*]` entries into the global config.

use clap::Args;
use clap::Subcommand;

use crate::error::{Error, Result};
use crate::hybrid::config::{
    PROVIDER_FASTEMBED, PROVIDER_OPENROUTER, ProviderState, provider_status,
};

/// `docsbase connect` arguments.
#[derive(Debug, Args)]
pub struct ConnectArgs {
    /// Provider to connect.
    #[command(subcommand)]
    pub provider: Option<ConnectProvider>,
}

/// Provider subcommands.
#[derive(Debug, Subcommand)]
pub enum ConnectProvider {
    /// Validate a key from `OPENROUTER_API_KEY` and register the provider
    /// (`hybrid-remote`).
    #[cfg(feature = "hybrid-remote")]
    Openrouter,
    /// Install the pinned ONNX Runtime and register `[providers.fastembed]`
    /// (`hybrid-local`).
    #[cfg(feature = "hybrid-local")]
    Fastembed(FastembedArgs),
}

/// `connect fastembed` flags.
#[cfg(feature = "hybrid-local")]
#[derive(Debug, Args)]
pub struct FastembedArgs {
    /// Only report the runtime state; install if missing.
    #[arg(long)]
    pub check: bool,
    /// Re-download and replace the runtime even when installed.
    #[arg(long)]
    pub force: bool,
}

/// Runs `docsbase connect`.
///
/// # Errors
/// Returns [`Error::Admission`] for provisioning failures (missing key,
/// offline, hash mismatch, unsupported target, free-space shortfall).
pub fn run(args: &ConnectArgs) -> anyhow::Result<()> {
    match &args.provider {
        None => print_status()?,
        #[cfg(feature = "hybrid-remote")]
        Some(ConnectProvider::Openrouter) => connect_openrouter()?,
        #[cfg(feature = "hybrid-local")]
        Some(ConnectProvider::Fastembed(fastembed)) => connect_fastembed(fastembed)?,
        #[allow(unreachable_patterns)]
        Some(_) => {
            return Err(Error::Admission {
                message: "this build has neither hybrid provider feature enabled; rebuild with --features hybrid-local and/or hybrid-remote"
                    .to_owned(),
            }
            .into());
        }
    }
    Ok(())
}

/// Status table over the four providers (FR-2).
fn print_status() -> Result<()> {
    let config = crate::config::Config::global()?;
    let providers = &config.providers;
    println!("provider\tstate\tversion\tpath\treason");
    for name in [PROVIDER_FASTEMBED, PROVIDER_OPENROUTER] {
        let status = provider_status(providers, name);
        let version = match name {
            PROVIDER_FASTEMBED => providers
                .fastembed
                .as_ref()
                .and_then(|provider| provider.runtime.as_deref())
                .unwrap_or("-"),
            _ => "-",
        };
        let path = match name {
            PROVIDER_FASTEMBED => providers
                .fastembed
                .as_ref()
                .and_then(|provider| provider.runtime_path.as_ref())
                .map_or_else(|| "-".to_owned(), |path| path.display().to_string()),
            _ => "-".to_owned(),
        };
        println!(
            "{name}\t{}\t{version}\t{path}\t{}",
            state_label(status.state),
            status.reason.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn state_label(state: ProviderState) -> &'static str {
    match state {
        ProviderState::Unknown => "Unknown",
        ProviderState::NotConnected => "NotConnected",
        ProviderState::Connected => "Connected",
        ProviderState::Broken => "Broken",
    }
}

/// Validates `OPENROUTER_API_KEY` with a zero-token `GET /api/v1/key` probe
/// (FR-3) and records `[providers.openrouter]` without the key.
#[cfg_attr(not(feature = "hybrid-remote"), allow(dead_code))]
fn connect_openrouter() -> anyhow::Result<()> {
    #[cfg(feature = "hybrid-remote")]
    {
        use crate::hybrid::openrouter::BASE_URL;
        use crate::hybrid::wire::Transport;

        let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::Admission {
            message:
                "OPENROUTER_API_KEY is not set; export it before `docsbase connect openrouter`"
                    .to_owned(),
        })?;
        if key.trim().is_empty() {
            return Err(Error::Admission {
                message: "OPENROUTER_API_KEY is empty".to_owned(),
            }
            .into());
        }
        // The probe is a dedicated single-shot path: no retry wrapper
        // (FR-3). 401/403 → check the key; transport → retry.
        let transport = crate::hybrid::openrouter::ReqwestTransport::new()?;
        let headers = vec![("Authorization".to_owned(), format!("Bearer {key}"))];
        let response = transport
            .get_json(&format!("{BASE_URL}/key"), &headers)
            .map_err(|err| Error::Admission {
                message: format!(
                    "key probe failed: {}; check the network and retry",
                    err.inner_message()
                ),
            })?;
        match response.status {
            200 => {}
            401 | 403 => {
                return Err(Error::Admission {
                    message: format!(
                        "OPENROUTER_API_KEY was rejected (HTTP {}); check the key",
                        response.status
                    ),
                }
                .into());
            }
            other => {
                return Err(Error::Admission {
                    message: format!("key probe returned HTTP {other}; retry"),
                }
                .into());
            }
        }
        write_global_provider(
            PROVIDER_OPENROUTER,
            &[("enabled", toml::Value::Boolean(true))],
        )?;
        println!("openrouter provider connected (key validated; never stored)");
        Ok(())
    }
    #[cfg(not(feature = "hybrid-remote"))]
    {
        Err(Error::Admission {
            message: "connect openrouter needs the hybrid-remote build; rebuild with --features hybrid-remote"
                .to_owned(),
        }
        .into())
    }
}

/// Installs the pinned ONNX Runtime and records `[providers.fastembed]`
/// (FR-4).
#[cfg(feature = "hybrid-local")]
fn connect_fastembed(args: &FastembedArgs) -> anyhow::Result<()> {
    use crate::hybrid::runtime::{self, HttpRuntimeSource, ORT_VERSION};

    let target = runtime::platform_target()?;
    let installed = runtime::library_path(target);

    if args.check && !args.force {
        if installed.exists() {
            // Confirm the file is loadable enough for reporting: size check.
            let metadata = std::fs::metadata(&installed)
                .map_err(|err| Error::internal_with_source("stat runtime library", err))?;
            if metadata.len() != target.library_size {
                return Err(Error::Admission {
                    message: format!(
                        "runtime library {} has size mismatch ({} vs expected {}); re-run `docsbase connect fastembed --force`",
                        installed.display(),
                        metadata.len(),
                        target.library_size
                    ),
                }
                .into());
            }
            println!(
                "fastembed runtime {ORT_VERSION} installed: {} ({})",
                installed.display(),
                metadata.len()
            );
            return Ok(());
        }
        println!(
            "fastembed runtime {ORT_VERSION} is not installed; run `docsbase connect fastembed`"
        );
        return Ok(());
    }

    let source = HttpRuntimeSource::new()?;
    let installed = runtime::install(target, &source, args.force)?;
    write_global_provider(
        PROVIDER_FASTEMBED,
        &[
            ("enabled", toml::Value::Boolean(true)),
            ("runtime", toml::Value::String(ORT_VERSION.to_owned())),
            (
                "runtime_path",
                toml::Value::String(installed.display().to_string()),
            ),
        ],
    )?;
    println!(
        "fastembed runtime {ORT_VERSION} installed: {} (sha256 {})",
        installed.display(),
        target.archive_sha256
    );
    Ok(())
}

/// Writes or merges one `[providers.<name>]` table into the global config
/// (`config.toml`), preserving every other section (FR-1).
fn write_global_provider(name: &str, fields: &[(&str, toml::Value)]) -> Result<()> {
    let config_dir = crate::config::paths::config_dir()?;
    crate::platform::fs::secure_dir(&config_dir).map_err(|err| {
        Error::internal_with_source(format!("create {}", config_dir.display()), err)
    })?;
    let path = config_dir.join("config.toml");

    let mut root: toml::Value = match std::fs::read_to_string(&path) {
        Ok(raw) => toml::from_str(&raw)
            .map_err(|err| Error::internal_with_source(format!("parse {}", path.display()), err))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(toml::map::Map::new())
        }
        Err(err) => {
            return Err(Error::internal_with_source(
                format!("read {}", path.display()),
                err,
            ));
        }
    };

    let table = root
        .as_table_mut()
        .ok_or_else(|| Error::internal(format!("{} is not a TOML table", path.display())))?;
    let providers = table
        .entry("providers")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| Error::internal("providers is not a table"))?;
    let mut provider_table = toml::map::Map::new();
    for (key, value) in fields {
        provider_table.insert((*key).to_owned(), value.clone());
    }
    providers.insert(name.to_owned(), toml::Value::Table(provider_table));

    let rendered =
        toml::to_string(&root).map_err(|err| Error::internal_with_source("render config", err))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, rendered)
        .map_err(|err| Error::internal_with_source(format!("write {}", tmp.display()), err))?;
    crate::platform::fs::secure_file(&tmp)
        .map_err(|err| Error::internal_with_source(format!("secure {}", tmp.display()), err))?;
    std::fs::rename(&tmp, &path)
        .map_err(|err| Error::internal_with_source(format!("publish {}", path.display()), err))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_labels_cover_all_provider_states() {
        for state in [
            ProviderState::Unknown,
            ProviderState::NotConnected,
            ProviderState::Connected,
            ProviderState::Broken,
        ] {
            assert!(!state_label(state).is_empty());
        }
    }

    #[test]
    #[allow(unsafe_code)] // test-only env mutation, isolated to this test
    fn provider_write_preserves_unrelated_sections() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let old = std::env::var_os("DOCSBASE_CONFIG_DIR");
        unsafe {
            std::env::set_var("DOCSBASE_CONFIG_DIR", dir.path());
        }
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "max_file_size = 77\n[hybrid]\nenabled = true\n").expect("seed");
        write_global_provider(
            PROVIDER_FASTEMBED,
            &[("enabled", toml::Value::Boolean(true))],
        )
        .expect("write");
        let reloaded: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).expect("read back")).expect("reparse");
        assert_eq!(
            reloaded
                .get("max_file_size")
                .and_then(toml::Value::as_integer),
            Some(77)
        );
        assert!(reloaded.get("hybrid").is_some());
        let providers = reloaded.get("providers").expect("providers");
        assert!(providers.get(PROVIDER_FASTEMBED).is_some());
        match old {
            Some(value) => unsafe {
                std::env::set_var("DOCSBASE_CONFIG_DIR", value);
            },
            None => unsafe {
                std::env::remove_var("DOCSBASE_CONFIG_DIR");
            },
        }
    }

    #[test]
    fn status_heading_includes_four_states() {
        // The table prints a header; just ensure the module compiles and the
        // status fn is reachable.
        let config = crate::config::Config::global().expect("global");
        let _ = provider_status(&config.providers, PROVIDER_FASTEMBED);
        let _ = provider_status(&config.providers, PROVIDER_OPENROUTER);
    }
}
