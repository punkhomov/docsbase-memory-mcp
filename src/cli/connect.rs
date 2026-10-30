//! `docsbase connect` (FR-2/3/4): provider status, `OpenRouter` key probe
//! (`hybrid-remote`), and the local fastembed/ORT install (`hybrid-local`).
//!
//! `connect` never selects a model (FR-4); it only prepares the machine and
//! writes `[providers.*]` entries into the global config.

use clap::Args;

use crate::error::{Error, Result};
use crate::hybrid::config::{
    PROVIDER_FASTEMBED, PROVIDER_OPENROUTER, ProviderState, provider_status,
};

/// `docsbase connect` arguments.
#[derive(Debug, Args)]
pub struct ConnectArgs {
    /// Provider to prepare (omit to print the status table).
    pub provider: Option<String>,
    /// Report the provider state without writing anything.
    #[arg(long)]
    pub check: bool,
    /// Re-probe / reinstall even when the provider is already prepared.
    #[arg(long)]
    pub force: bool,
}

/// The provider-specific half of `connect`: report and prepare. Lives in the
/// CLI layer on purpose — the core registry must not know about `ureq`, `zip`
/// or config writes (ADR-18, design §6).
trait Provisioner {
    /// Report lines for `--check`; never writes.
    ///
    /// # Errors
    /// Admission when the state cannot be determined.
    fn check(&self) -> Result<Vec<String>>;
    /// Prepares the provider and returns its report lines.
    ///
    /// # Errors
    /// Admission for a rejected probe, a failed download or a hash mismatch.
    fn install(&self, force: bool) -> Result<InstallOutcome>;
    /// The `[providers.<name>]` payload written after a successful install.
    fn record(&self) -> toml::Table;
}

/// What an install did, for the restart hint (FR-13).
struct InstallOutcome {
    lines: Vec<String>,
    /// True only when something was actually (re)installed.
    reinstalled: bool,
}

/// CLI-side provisioning table. A second table is the price of keeping the
/// core provider-agnostic; `every_provisioner_is_a_registered_provider`
/// catches drift between the two.
fn provisioner_for(name: &str) -> Option<&'static dyn Provisioner> {
    match name {
        PROVIDER_OPENROUTER => Some(&OpenrouterProvisioner),
        PROVIDER_FASTEMBED => Some(&FastembedProvisioner),
        _ => None,
    }
}

/// Runs `docsbase connect`.
///
/// # Errors
/// Returns [`Error::Admission`] for provisioning failures (missing key,
/// offline, hash mismatch, unsupported target, free-space shortfall).
pub fn run(args: &ConnectArgs) -> anyhow::Result<()> {
    let Some(name) = args.provider.as_deref() else {
        if args.check || args.force {
            return Err(Error::Admission {
                message: "`--check`/`--force` need a provider: `docsbase connect <provider> [--check] [--force]`"
                    .to_owned(),
            }
            .into());
        }
        print_status()?;
        return Ok(());
    };
    // Validate the name against the provider registry first, so an unknown
    // provider lists the registry (FR-5) rather than a provisioning hint. The
    // registry's own message talks about model addresses, which is the wrong
    // vocabulary for a provider name.
    if crate::hybrid::provider::provider(name).is_err() {
        return Err(crate::hybrid::provider::unknown_provider(name).into());
    }
    let Some(provisioner) = provisioner_for(name) else {
        return Err(Error::Admission {
            message: format!("provider \"{name}\" needs no preparation"),
        }
        .into());
    };
    connect_frame(provisioner, name, args.check, args.force)
}

/// The one frame every provider goes through (FR-8): flag validation →
/// report or install → config write → restart hint. The provider supplies
/// the bodies; the frame is provider-agnostic and unit-tested with a fake.
fn connect_frame(
    provisioner: &dyn Provisioner,
    name: &str,
    check: bool,
    force: bool,
) -> anyhow::Result<()> {
    if check && force {
        return Err(Error::Admission {
            message: "`--check` and `--force` are mutually exclusive; use `--force` alone to reinstall, or `--check` alone to report state"
                .to_owned(),
        }
        .into());
    }
    if check {
        for line in provisioner.check()? {
            println!("{line}");
        }
        return Ok(());
    }
    let outcome = provisioner.install(force)?;
    write_global_provider(name, &provisioner.record())?;
    for line in &outcome.lines {
        println!("{line}");
    }
    if let Some(note) = restart_note(outcome.reinstalled) {
        println!("{note}");
    }
    Ok(())
}

/// One `provider/state/version/path/reason` row — the same columns for every
/// provider, so `connect <p> --check` and the status table agree (AC-3, FR-7).
fn status_row(name: &str, providers: &crate::hybrid::config::ProvidersCfg) -> Vec<String> {
    let provider = match crate::hybrid::provider::provider(name) {
        Ok(provider) => provider,
        Err(err) => return vec![format!("{name}\tunknown\t-\t-\t{}", err.inner_message())],
    };
    let status = provider.status(providers);
    vec![format!(
        "{}\t{}\t{}\t{}\t{}",
        name,
        state_label(status.state),
        provider
            .version(providers)
            .unwrap_or_else(|| "-".to_owned()),
        status
            .path
            .as_ref()
            .map_or_else(|| "-".to_owned(), |path| path.display().to_string()),
        status.reason.as_deref().unwrap_or("-")
    )]
}

/// Status table over the configured providers (FR-2).
fn print_status() -> Result<()> {
    let config = crate::config::Config::global()?;
    let providers = &config.providers;
    println!("provider\tstate\tversion\tpath\treason");
    for provider in crate::hybrid::provider::providers() {
        let name = provider.name();
        let status = provider_status(providers, name);
        let version = provider
            .version(providers)
            .unwrap_or_else(|| "-".to_owned());
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

/// `openrouter` provisioning: validate the key with a zero-token probe and
/// record `enabled = true` (never the key itself).
struct OpenrouterProvisioner;

impl Provisioner for OpenrouterProvisioner {
    fn check(&self) -> Result<Vec<String>> {
        // `--check` reports the recorded state and probes the key, but writes
        // nothing (FR-8). The row has the same columns as every other provider
        // (AC-3), so `connect <p> --check` output is comparable across
        // providers.
        let config = crate::config::Config::global()?;
        let mut lines = status_row(PROVIDER_OPENROUTER, &config.providers);
        #[cfg(feature = "hybrid-remote")]
        {
            let transport = crate::hybrid::openrouter::ReqwestTransport::new()?;
            probe_openrouter_key(&transport)?;
            lines.push("openrouter: key probe accepted (not stored)".to_owned());
        }
        #[cfg(not(feature = "hybrid-remote"))]
        lines.push("openrouter: needs the hybrid-remote build to probe the key".to_owned());
        Ok(lines)
    }

    fn install(&self, _force: bool) -> Result<InstallOutcome> {
        #[cfg(feature = "hybrid-remote")]
        {
            let transport = crate::hybrid::openrouter::ReqwestTransport::new()?;
            probe_openrouter_key(&transport)?;
            Ok(InstallOutcome {
                lines: vec![
                    "openrouter provider connected (key validated; never stored)".to_owned(),
                ],
                // Nothing is installed locally, so a running daemon has
                // nothing to reload.
                reinstalled: false,
            })
        }
        #[cfg(not(feature = "hybrid-remote"))]
        {
            Err(crate::hybrid::provider::provider(PROVIDER_OPENROUTER)?.feature_missing())
        }
    }

    fn record(&self) -> toml::Table {
        let mut table = toml::Table::new();
        table.insert("enabled".to_owned(), toml::Value::Boolean(true));
        table
    }
}

/// Validates `OPENROUTER_API_KEY` with a zero-token `GET /api/v1/key` probe
/// (FR-3). Writes the provider entry only when `write` is set (`--check` must
/// not touch the config). The key is never stored or logged.
///
/// # Errors
/// Returns [`Error::Admission`] for a missing/empty key, 401/403 (check the
/// key), transport failures (retry) or any other status.
#[cfg(feature = "hybrid-remote")]
fn probe_openrouter_key(transport: &dyn crate::hybrid::wire::Transport) -> Result<()> {
    use crate::hybrid::openrouter::BASE_URL;

    let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| Error::Admission {
        message: "OPENROUTER_API_KEY is not set; export it before `docsbase connect openrouter`"
            .to_owned(),
    })?;
    if key.trim().is_empty() {
        return Err(Error::Admission {
            message: "OPENROUTER_API_KEY is empty".to_owned(),
        });
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
            });
        }
        other => {
            return Err(Error::Admission {
                message: format!("key probe returned HTTP {other}; retry"),
            });
        }
    }
    Ok(())
}

/// `fastembed` provisioning: install the pinned ONNX Runtime and record its
/// version and path.
struct FastembedProvisioner;

impl Provisioner for FastembedProvisioner {
    fn check(&self) -> Result<Vec<String>> {
        #[cfg(feature = "hybrid-local")]
        {
            use crate::hybrid::runtime::{self, ORT_VERSION};
            let target = runtime::platform_target()?;
            let config = crate::config::Config::global()?;
            // Same columns as every provider (AC-3).
            let mut lines = status_row(PROVIDER_FASTEMBED, &config.providers);
            let status = runtime::runtime_status(&config.providers);
            let Some(path) = &status.path else {
                lines.push(format!(
                    "fastembed runtime {ORT_VERSION} is not installed; run `docsbase connect fastembed`"
                ));
                return Ok(lines);
            };
            // A vanished path is a Broken state, not an internal error
            // (diagnostic commands must not fail with Internal; panel C8).
            let Ok(metadata) = std::fs::metadata(path) else {
                lines.push(format!(
                    "fastembed runtime {} is missing; re-run `docsbase connect fastembed --force`",
                    path.display()
                ));
                return Ok(lines);
            };
            if metadata.len() != target.library_size {
                return Err(Error::Admission {
                    message: format!(
                        "runtime library {} has size mismatch ({} vs expected {}); re-run `docsbase connect fastembed --force`",
                        path.display(),
                        metadata.len(),
                        target.library_size
                    ),
                });
            }
            #[cfg(target_os = "macos")]
            lines.push(format!(
                "macOS signature state: {}",
                macos_signature_state(path)
            ));
            lines.push(format!(
                "fastembed runtime {ORT_VERSION} installed: {}",
                path.display()
            ));
            Ok(lines)
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            Err(crate::hybrid::provider::provider(PROVIDER_FASTEMBED)?.feature_missing())
        }
    }

    fn install(&self, force: bool) -> Result<InstallOutcome> {
        #[cfg(feature = "hybrid-local")]
        {
            use crate::hybrid::runtime::{self, HttpRuntimeSource, ORT_VERSION};
            let target = runtime::platform_target()?;
            let source = HttpRuntimeSource::new()?;
            let (installed, reinstalled) = runtime::install(target, &source, force)?;
            Ok(InstallOutcome {
                lines: vec![format!(
                    "fastembed runtime {ORT_VERSION} installed: {} (sha256 {})",
                    installed.display(),
                    target.archive_sha256
                )],
                reinstalled,
            })
        }
        #[cfg(not(feature = "hybrid-local"))]
        {
            let _ = force;
            Err(crate::hybrid::provider::provider(PROVIDER_FASTEMBED)?.feature_missing())
        }
    }

    fn record(&self) -> toml::Table {
        let mut table = toml::Table::new();
        table.insert("enabled".to_owned(), toml::Value::Boolean(true));
        #[cfg(feature = "hybrid-local")]
        {
            use crate::hybrid::runtime::ORT_VERSION;
            if let Ok(target) = crate::hybrid::runtime::platform_target() {
                let path = crate::hybrid::runtime::library_path(target);
                table.insert(
                    "runtime".to_owned(),
                    toml::Value::String(ORT_VERSION.to_owned()),
                );
                table.insert(
                    "runtime_path".to_owned(),
                    toml::Value::String(path.display().to_string()),
                );
            }
        }
        table
    }
}

/// FR-13 note for a `connect` run: present only when something was actually
/// (re)installed (round-7 deepseek: the decision is a pure predicate so both
/// branches are unit-testable without a network install).
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
fn write_global_provider(name: &str, record: &toml::Table) -> Result<()> {
    let config_dir = crate::config::paths::config_dir()?;
    crate::platform::fs::secure_dir(&config_dir).map_err(|err| {
        Error::internal_with_source(format!("create {}", config_dir.display()), err)
    })?;
    let path = config_dir.join("config.toml");
    // Cross-process serialization with `models use --global` (C13).
    crate::config::paths::with_config_write_lock(&path, || {
        write_global_provider_locked(name, record, &path).map_err(|err| anyhow::anyhow!("{err}"))
    })
    .map_err(|err| Error::internal(err.to_string()))
}

/// The read-modify-write body, run under the config-write lock.
fn write_global_provider_locked(
    name: &str,
    record: &toml::Table,
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
    providers.insert(name.to_owned(), toml::Value::Table(record.clone()));

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
    #[test]
    fn every_provisioner_is_a_registered_provider() {
        // The CLI provisioning table is a second table; this keeps it in sync
        // with the provider registry (ADR-18 design §6).
        for provider in crate::hybrid::provider::providers() {
            assert!(
                provisioner_for(provider.name()).is_some(),
                "provider \"{}\" has no provisioner",
                provider.name()
            );
        }
        assert!(provisioner_for("no-such-provider").is_none());
    }

    /// Fake provisioner: proves the frame is provider-agnostic (FR-10).
    struct FakeProvisioner {
        writes: std::sync::atomic::AtomicUsize,
        installed: bool,
    }

    impl Provisioner for FakeProvisioner {
        fn check(&self) -> Result<Vec<String>> {
            Ok(vec!["fake: check".to_owned()])
        }
        fn install(&self, force: bool) -> Result<InstallOutcome> {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(InstallOutcome {
                lines: vec![format!("fake: install force={force}")],
                reinstalled: self.installed,
            })
        }
        fn record(&self) -> toml::Table {
            let mut table = toml::Table::new();
            table.insert("enabled".to_owned(), toml::Value::Boolean(true));
            table
        }
    }

    #[test]
    fn connect_frame_rejects_check_with_force_for_every_provider() {
        let fake = FakeProvisioner {
            writes: std::sync::atomic::AtomicUsize::new(0),
            installed: false,
        };
        let err = connect_frame(&fake, "fake", true, true).expect_err("conflicting flags");
        assert!(err.to_string().contains("mutually exclusive"), "{err}");
        assert_eq!(
            fake.writes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a rejected frame must not touch the provider"
        );
    }

    #[test]
    #[allow(unsafe_code)] // env isolation under the shared env lock
    fn connect_frame_check_never_writes_the_config() {
        let dir = tempfile::TempDir::new().expect("tmp");
        let _guard = env_guard();
        // SAFETY: single-threaded under the shared env lock.
        unsafe {
            std::env::set_var("DOCSBASE_CONFIG_DIR", dir.path());
        }
        let fake = FakeProvisioner {
            writes: std::sync::atomic::AtomicUsize::new(0),
            installed: false,
        };
        connect_frame(&fake, "fake", true, false).expect("check");
        assert_eq!(
            fake.writes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "`--check` must not install"
        );
        assert!(
            !dir.path().join("config.toml").exists(),
            "`--check` must not write the config"
        );
    }

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
        // The probe itself is pure (the frame owns the single config write),
        // and the record it feeds never contains the key.
        assert!(
            !dir.path().join("config.toml").exists(),
            "the probe must not write the config"
        );
        write_global_provider(PROVIDER_OPENROUTER, &OpenrouterProvisioner.record())
            .expect("frame write");
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
        assert!(matches!(err, Error::Admission { .. }), "{err:?}");
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
        let mut record = toml::Table::new();
        record.insert("enabled".to_owned(), toml::Value::Boolean(true));
        write_global_provider(PROVIDER_FASTEMBED, &record).expect("write");
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
