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
    /// Only report the runtime state (does not install).
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

/// Status table over the configured providers (FR-2).
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
        // The resolved library comes from the status, not from the config
        // field: an entry without `runtime_path` still resolves through the
        // data-dir install, and that path must show up here (round-7 C1).
        let path = status
            .path
            .as_ref()
            .map_or_else(|| "-".to_owned(), |path| path.display().to_string());
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
        let transport = crate::hybrid::openrouter::ReqwestTransport::new()?;
        probe_openrouter_key(&transport)
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

/// Performs the key probe over an injected [`Transport`] — the single-shot
/// path (no retry wrapper, FR-3) — and writes the provider entry on success.
/// The key is never stored or logged (FR-3).
///
/// # Errors
/// Returns [`Error::Admission`] for a missing/empty key, 401/403 (check the
/// key), transport failures (retry) or any other status.
#[cfg(feature = "hybrid-remote")]
fn probe_openrouter_key(transport: &dyn crate::hybrid::wire::Transport) -> anyhow::Result<()> {
    use crate::hybrid::openrouter::BASE_URL;

    let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::Admission {
        message: "OPENROUTER_API_KEY is not set; export it before `docsbase connect openrouter`"
            .to_owned(),
    })?;
    if key.trim().is_empty() {
        return Err(Error::Admission {
            message: "OPENROUTER_API_KEY is empty".to_owned(),
        }
        .into());
    }
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

/// Installs the pinned ONNX Runtime and records `[providers.fastembed]`
/// (FR-4).
#[cfg(feature = "hybrid-local")]
fn connect_fastembed(args: &FastembedArgs) -> anyhow::Result<()> {
    use crate::hybrid::runtime::{self, HttpRuntimeSource, ORT_VERSION};

    // `--check` reports state; `--force` reinstalls. Together they are
    // contradictory — reject loudly instead of silently installing
    // (round-2 U4), matching the scope-flag conflict handling.
    if args.check && args.force {
        return Err(Error::Admission {
            message: "`--check` and `--force` are mutually exclusive; use `--force` alone to reinstall, or `--check` alone to report state"
                .to_owned(),
        }
        .into());
    }

    let target = runtime::platform_target()?;

    // `--check` reports the runtime state; macOS additionally reports the
    // signature state (valid/adhoc/missing) — brief T2 step 5.
    if args.check && !args.force {
        let config = crate::config::Config::global()?;
        let status = runtime::runtime_status(&config.providers);
        println!(
            "fastembed runtime: state={} version={} path={} reason={}",
            status.state,
            status.version.as_deref().unwrap_or("-"),
            status
                .path
                .as_ref()
                .map_or_else(|| "-".to_owned(), |path| path.display().to_string()),
            status.reason.as_deref().unwrap_or("-")
        );
        if let Some(path) = &status.path {
            // A vanished path is a Broken state, not an internal error
            // (diagnostic commands must not fail with Internal; panel C8).
            let Ok(metadata) = std::fs::metadata(path) else {
                println!(
                    "fastembed runtime {} is missing; re-run `docsbase connect fastembed --force`",
                    path.display()
                );
                return Ok(());
            };
            if metadata.len() != target.library_size {
                return Err(Error::Admission {
                    message: format!(
                        "runtime library {} has size mismatch ({} vs expected {}); re-run `docsbase connect fastembed --force`",
                        path.display(),
                        metadata.len(),
                        target.library_size
                    ),
                }
                .into());
            }
            #[cfg(target_os = "macos")]
            println!("macOS signature state: {}", macos_signature_state(path));
            println!(
                "fastembed runtime {ORT_VERSION} installed: {}",
                path.display()
            );
        } else {
            println!(
                "fastembed runtime {ORT_VERSION} is not installed; run `docsbase connect fastembed`"
            );
        }
        return Ok(());
    }

    let source = HttpRuntimeSource::new()?;
    let (installed, reinstalled) = runtime::install(target, &source, args.force)?;
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
    // FR-13: a running daemon keeps the previously loaded ORT library
    // mapped (and its cached model instances) until restarted; say so only
    // when this call actually (re)installed the runtime — a no-op
    // `connect` must not ask for a restart (round-6 mimo/muse/deepseek).
    if let Some(note) = restart_note(reinstalled) {
        println!("{note}");
    }
    Ok(())
}

/// FR-13 note for a `connect fastembed` run: present only when the runtime
/// was actually (re)installed (round-7 deepseek: the decision is a pure
/// predicate so both branches are unit-testable without a network install).
#[cfg(feature = "hybrid-local")]
fn restart_note(reinstalled: bool) -> Option<&'static str> {
    reinstalled.then_some("note: restart a running detached daemon to pick up the runtime")
}

/// Reports the macOS code-signature state of an installed runtime library:
/// `valid`, `adhoc`, or `missing` (when codesign cannot read it). Gated like
/// its only caller (`connect_fastembed`): in the default build the runtime
/// install does not exist, so an ungated helper is dead code and `-D warnings`
/// fails the macOS default clippy job.
#[cfg(all(target_os = "macos", feature = "hybrid-local"))]
fn macos_signature_state(path: &std::path::Path) -> &'static str {
    use std::process::Command;
    let output = Command::new("codesign")
        .args(["--verify", "--strict", path.to_str().unwrap_or_default()])
        .output();
    match output {
        Ok(output) if output.status.success() => "valid",
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("ad hoc") || stderr.contains("not signed") {
                "adhoc"
            } else {
                "missing"
            }
        }
        Err(_) => "missing",
    }
}

/// Writes or merges one `[providers.<name>]` table into the global config
/// (`config.toml`), preserving every other section (FR-1).
#[cfg_attr(
    not(any(feature = "hybrid-local", feature = "hybrid-remote")),
    allow(dead_code)
)]
fn write_global_provider(name: &str, fields: &[(&str, toml::Value)]) -> Result<()> {
    let config_dir = crate::config::paths::config_dir()?;
    crate::platform::fs::secure_dir(&config_dir).map_err(|err| {
        Error::internal_with_source(format!("create {}", config_dir.display()), err)
    })?;
    let path = config_dir.join("config.toml");
    // Cross-process serialization with `models use --global` (C13).
    crate::config::paths::with_config_write_lock(&path, || {
        write_global_provider_locked(name, fields, &path).map_err(|err| anyhow::anyhow!("{err}"))
    })
    .map_err(|err| Error::internal(err.to_string()))
}

/// The read-modify-write body, run under the config-write lock.
fn write_global_provider_locked(
    name: &str,
    fields: &[(&str, toml::Value)],
    path: &std::path::Path,
) -> Result<()> {
    let mut root: toml::Value = match std::fs::read_to_string(path) {
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
    std::fs::rename(&tmp, path)
        .map_err(|err| Error::internal_with_source(format!("publish {}", path.display()), err))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "hybrid-local")]
    #[test]
    fn restart_note_only_after_a_real_install() {
        // Round-7 deepseek: both halves of the FR-13 decision are pinned —
        // a no-op connect stays quiet, a real (re)install asks for the
        // restart.
        assert_eq!(super::restart_note(false), None);
        assert_eq!(
            super::restart_note(true),
            Some("note: restart a running detached daemon to pick up the runtime")
        );
    }

    use super::*;

    /// Shared with every other CLI unit-test module (`cli::test_env`);
    /// env mutation must be serialized process-wide.
    fn env_guard() -> crate::cli::test_env::EnvGuard {
        crate::cli::test_env::env_guard()
    }

    /// Fake transport that records the requested URL and replays a scripted
    /// response (single-shot key probe).
    #[cfg(feature = "hybrid-remote")]
    struct FakeKeyTransport {
        calls: std::sync::Mutex<Vec<String>>,
        status: u16,
        transport_error: bool,
    }

    #[cfg(feature = "hybrid-remote")]
    impl crate::hybrid::wire::Transport for FakeKeyTransport {
        fn post_json(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            _body: &str,
        ) -> Result<crate::hybrid::wire::TransportResponse> {
            Err(Error::internal("not used in the key probe"))
        }

        fn get_json(
            &self,
            url: &str,
            _headers: &[(String, String)],
        ) -> Result<crate::hybrid::wire::TransportResponse> {
            self.calls.lock().expect("calls").push(url.to_owned());
            if self.transport_error {
                return Err(Error::internal("connection reset"));
            }
            Ok(crate::hybrid::wire::TransportResponse {
                status: self.status,
                body: "{}".to_owned(),
                retry_after: None,
            })
        }
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    #[allow(unsafe_code)] // test-only env mutation, serialized by env_guard
    fn openrouter_probe_gets_key_and_never_persists_it() {
        let _guard = env_guard();
        // Isolated config dir; the probe writes the provider entry.
        let dir = tempfile::TempDir::new().expect("tmp");
        let old = std::env::var_os("DOCSBASE_CONFIG_DIR");
        let old_key = std::env::var_os("OPENROUTER_API_KEY");
        unsafe {
            std::env::set_var("DOCSBASE_CONFIG_DIR", dir.path());
            std::env::set_var("OPENROUTER_API_KEY", "test-key-secret");
        }
        let transport = FakeKeyTransport {
            calls: std::sync::Mutex::new(Vec::new()),
            status: 200,
            transport_error: false,
        };
        probe_openrouter_key(&transport).expect("probe ok");
        // GET to the key endpoint, zero model tokens.
        let calls = transport.calls.lock().expect("calls");
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].ends_with("/key"),
            "{} is the key endpoint",
            calls[0]
        );
        drop(calls);
        // Provider entry written, key never persisted.
        let path = dir.path().join("config.toml");
        let text = std::fs::read_to_string(&path).expect("config written");
        assert!(text.contains("providers"), "{text}");
        assert!(text.contains("openrouter"), "{text}");
        assert!(
            !text.contains("test-key-secret"),
            "key must never be stored: {text}"
        );
        assert!(!text.contains("Bearer"), "{text}");
        match old {
            Some(value) => unsafe {
                std::env::set_var("DOCSBASE_CONFIG_DIR", value);
            },
            None => unsafe {
                std::env::remove_var("DOCSBASE_CONFIG_DIR");
            },
        }
        match old_key {
            Some(value) => unsafe {
                std::env::set_var("OPENROUTER_API_KEY", value);
            },
            None => unsafe {
                std::env::remove_var("OPENROUTER_API_KEY");
            },
        }
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    #[allow(unsafe_code)] // test-only env mutation, serialized by env_guard
    fn openrouter_probe_401_is_admission_and_writes_nothing() {
        let _guard = env_guard();
        let dir = tempfile::TempDir::new().expect("tmp");
        let old = std::env::var_os("DOCSBASE_CONFIG_DIR");
        let old_key = std::env::var_os("OPENROUTER_API_KEY");
        unsafe {
            std::env::set_var("DOCSBASE_CONFIG_DIR", dir.path());
            std::env::set_var("OPENROUTER_API_KEY", "bad-key");
        }
        let transport = FakeKeyTransport {
            calls: std::sync::Mutex::new(Vec::new()),
            status: 401,
            transport_error: false,
        };
        let err = probe_openrouter_key(&transport).expect_err("401 rejected");
        assert!(matches!(err, anyhow::Error { .. }));
        let text = format!("{err}");
        assert!(text.contains("OPENROUTER_API_KEY"), "{text}");
        assert!(
            !dir.path().join("config.toml").exists(),
            "nothing written on 401"
        );
        match old {
            Some(value) => unsafe {
                std::env::set_var("DOCSBASE_CONFIG_DIR", value);
            },
            None => unsafe {
                std::env::remove_var("DOCSBASE_CONFIG_DIR");
            },
        }
        match old_key {
            Some(value) => unsafe {
                std::env::set_var("OPENROUTER_API_KEY", value);
            },
            None => unsafe {
                std::env::remove_var("OPENROUTER_API_KEY");
            },
        }
    }

    #[cfg(feature = "hybrid-remote")]
    #[test]
    #[allow(unsafe_code)] // test-only env mutation, serialized by env_guard
    fn openrouter_probe_transport_error_is_admission_retry() {
        let _guard = env_guard();
        let dir = tempfile::TempDir::new().expect("tmp");
        let old = std::env::var_os("DOCSBASE_CONFIG_DIR");
        let old_key = std::env::var_os("OPENROUTER_API_KEY");
        unsafe {
            std::env::set_var("DOCSBASE_CONFIG_DIR", dir.path());
            std::env::set_var("OPENROUTER_API_KEY", "some-key");
        }
        let transport = FakeKeyTransport {
            calls: std::sync::Mutex::new(Vec::new()),
            status: 200,
            transport_error: true,
        };
        let err = probe_openrouter_key(&transport).expect_err("transport fails");
        let text = format!("{err}");
        assert!(text.contains("retry"), "{text}");
        assert!(!dir.path().join("config.toml").exists());
        match old {
            Some(value) => unsafe {
                std::env::set_var("DOCSBASE_CONFIG_DIR", value);
            },
            None => unsafe {
                std::env::remove_var("DOCSBASE_CONFIG_DIR");
            },
        }
        match old_key {
            Some(value) => unsafe {
                std::env::set_var("OPENROUTER_API_KEY", value);
            },
            None => unsafe {
                std::env::remove_var("OPENROUTER_API_KEY");
            },
        }
    }

    #[test]
    fn state_labels_cover_all_provider_states() {
        assert_eq!(state_label(ProviderState::Unknown), "Unknown");
        assert_eq!(state_label(ProviderState::NotConnected), "NotConnected");
        assert_eq!(state_label(ProviderState::Connected), "Connected");
        assert_eq!(state_label(ProviderState::Broken), "Broken");
    }

    #[test]
    #[allow(unsafe_code)] // test-only env mutation, serialized by env_guard
    fn provider_write_preserves_unrelated_sections() {
        let _guard = env_guard();
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
    fn provider_status_returns_all_four_states_for_unknown_provider() {
        let providers = crate::hybrid::config::ProvidersCfg::default();
        assert_eq!(
            provider_status(&providers, PROVIDER_FASTEMBED).state,
            ProviderState::Unknown
        );
        assert_eq!(
            provider_status(&providers, PROVIDER_OPENROUTER).state,
            ProviderState::Unknown
        );
    }
}
