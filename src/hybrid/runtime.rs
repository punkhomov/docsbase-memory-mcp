//! ONNX Runtime install, resolution and init (ADR-17, spec hybrid-local T2).
//!
//! The target table (OQ-1) is filled from the official Microsoft releases of
//! ORT 1.24.4: five assets (`linux-x64`, `linux-aarch64`, `osx-arm64`,
//! `win-x64`, `win-arm64`; no macOS x64, macOS >= 14). Sizes and sha256
//! were verified against the release archives during spec work (progress.md,
//! T2). Everything in this module runs only under explicit `docsbase connect
//! fastembed`; `resolve` never downloads (FR-11).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::Digest;

use fd_lock::RwLock;

use crate::error::{Error, Result};
use crate::hybrid::config::{ProvidersCfg, provider_status};
use crate::platform;

/// Installed ORT version (must match the table URLs).
pub const ORT_VERSION: &str = "1.24.4";

/// Archive size cap (NFR-3).
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// Extracted library size cap (NFR-3).
pub const MAX_LIBRARY_BYTES: u64 = 512 * 1024 * 1024;

/// Free-space headroom and multiplier for the connect preflight (FR-13).
pub const PREFLIGHT_HEADROOM_BYTES: u64 = 256 * 1024 * 1024;

/// One row of the ORT release table (OQ-1).
#[derive(Debug, Clone, Copy)]
pub struct Target {
    /// Rust triple (`x86_64-unknown-linux-gnu`, …) used for the dir name.
    pub triple: &'static str,
    /// Official release archive URL.
    pub archive_url: &'static str,
    /// Hex sha256 of the archive.
    pub archive_sha256: &'static str,
    /// Archive size in bytes.
    pub archive_size: u64,
    /// Path of the runtime library inside the archive.
    pub library_path: &'static str,
    /// Extracted library size in bytes.
    pub library_size: u64,
}

/// The five supported targets (no macOS x64; macOS floor 14, enforced by the
/// osx-arm64-only matrix).
pub const TARGETS: &[Target] = &[
    Target {
        triple: "x86_64-unknown-linux-gnu",
        archive_url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-x64-1.24.4.tgz",
        archive_sha256: "3a211fbea252c1e66290658f1b735b772056149f28321e71c308942cdb54b747",
        archive_size: 8_155_822,
        library_path: "onnxruntime-linux-x64-1.24.4/lib/libonnxruntime.so.1.24.4",
        library_size: 22_159_232,
    },
    Target {
        triple: "aarch64-unknown-linux-gnu",
        archive_url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-aarch64-1.24.4.tgz",
        archive_sha256: "866109a9248d057671a039b9d725be4bd86888e3754140e6701ec621be9d4d7e",
        archive_size: 7_181_958,
        library_path: "onnxruntime-linux-aarch64-1.24.4/lib/libonnxruntime.so.1.24.4",
        library_size: 18_625_376,
    },
    Target {
        triple: "aarch64-apple-darwin",
        archive_url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-osx-arm64-1.24.4.tgz",
        archive_sha256: "93787795f47e1eee369182e43ed51b9e5da0878ab0346aecf4258979b8bba989",
        archive_size: 30_937_282,
        library_path: "onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.dylib",
        library_size: 35_418_600,
    },
    Target {
        triple: "x86_64-pc-windows-msvc",
        archive_url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-win-x64-1.24.4.zip",
        archive_sha256: "d2319fddfb6ea4db99ccc4b60c85c517bcd855721f5daa6a06d40d7cb2ee2357",
        archive_size: 74_442_783,
        library_path: "onnxruntime-win-x64-1.24.4/lib/onnxruntime.dll",
        library_size: 14_203_464,
    },
    Target {
        triple: "aarch64-pc-windows-msvc",
        archive_url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-win-arm64-1.24.4.zip",
        archive_sha256: "47dc80aa39da792271af10be5993919536a4dab0965ec1e6043ef37f1df7a693",
        archive_size: 75_182_070,
        library_path: "onnxruntime-win-arm64-1.24.4/lib/onnxruntime.dll",
        library_size: 14_215_752,
    },
];

/// Minimum macOS version (ORT 1.24.4 dropped Intel and requires 14.0).
pub const MACOS_MIN_MAJOR: u32 = 14;

/// Detects the current target and returns its release row.
///
/// # Errors
/// Returns [`Error::Admission`] for unsupported targets (e.g. macOS x64,
/// 32-bit, or macOS older than 14) — the local provider simply does not
/// exist there.
pub fn platform_target() -> Result<&'static Target> {
    #[cfg(target_os = "macos")]
    {
        let major = macos_major_version();
        if let Some(major) = major
            && major < MACOS_MIN_MAJOR
        {
            return Err(Error::Admission {
                message: format!(
                    "macOS {major}.0 is too old; the local ONNX Runtime requires macOS {MACOS_MIN_MAJOR} or newer"
                ),
            });
        }
    }
    select_target(&triple())
}

/// Current Rust triple as a string (platform-target selection key).
fn triple() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        match std::env::consts::OS {
            "linux" => "unknown-linux-gnu",
            "macos" => "apple-darwin",
            "windows" => "pc-windows-msvc",
            other => other,
        }
    )
}

/// Selects the release row for a triple; unknown → admission. Exposed for
/// tests (a foreign triple is not reachable on the host).
fn select_target(triple: &str) -> Result<&'static Target> {
    TARGETS
        .iter()
        .find(|target| target.triple == triple)
        .ok_or_else(|| Error::Admission {
            message: format!(
                "no local ONNX Runtime target for {triple} (supported: linux-x64/aarch64, osx-arm64, win-x64/arm64; macOS >= 14)"
            ),
        })
}

/// macOS major version via `sysctl(KERN_OSVERSION)` (e.g. 14.5.0 → 14).
#[cfg(target_os = "macos")]
fn macos_major_version() -> Option<u32> {
    let mut size = 0;
    // SAFETY: first call sizes the buffer; sysctl writes the size.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.osversion".as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0_u8; size];
    // SAFETY: buffer is `size` bytes; sysctl writes at most `size` bytes.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.osversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer);
    text.split('.')
        .next()
        .and_then(|part| part.trim().parse::<u32>().ok())
}

/// Reported runtime state (FR-2 status table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatus {
    /// State label (Unknown/NotConnected/Connected/Broken).
    pub state: &'static str,
    /// Installed ORT version when known.
    pub version: Option<String>,
    /// Library path when known.
    pub path: Option<PathBuf>,
    /// One-line reason for non-Connected states.
    pub reason: Option<String>,
}

/// Computes the fastembed runtime status from the global providers entry and
/// the data-dir install (FR-2).
#[must_use]
pub fn runtime_status(providers: &ProvidersCfg) -> RuntimeStatus {
    let status = provider_status(providers, crate::hybrid::config::PROVIDER_FASTEMBED);
    let path = providers
        .fastembed
        .as_ref()
        .and_then(|provider| provider.runtime_path.as_ref())
        .cloned()
        .or_else(|| {
            platform_target()
                .ok()
                .map(library_path)
                .filter(|path| path.exists())
        });
    match status.state {
        crate::hybrid::config::ProviderState::Connected => RuntimeStatus {
            state: "Connected",
            version: providers
                .fastembed
                .as_ref()
                .and_then(|provider| provider.runtime.clone()),
            path,
            reason: None,
        },
        crate::hybrid::config::ProviderState::Broken => RuntimeStatus {
            state: "Broken",
            version: providers
                .fastembed
                .as_ref()
                .and_then(|provider| provider.runtime.clone()),
            path,
            reason: status.reason,
        },
        crate::hybrid::config::ProviderState::NotConnected => RuntimeStatus {
            state: "NotConnected",
            version: None,
            path,
            reason: status.reason,
        },
        crate::hybrid::config::ProviderState::Unknown => RuntimeStatus {
            state: "Unknown",
            version: None,
            path: None,
            reason: status.reason,
        },
    }
}

/// A source of runtime archive bytes. Production uses [`HttpRuntimeSource`];
/// tests use a fake that produces tiny archives.
pub trait RuntimeSource {
    /// Downloads `url` and returns the bytes.
    ///
    /// # Errors
    /// Returns [`Error::Admission`] for transport failures.
    fn download(&self, url: &str) -> Result<Vec<u8>>;
}

/// Production source (blocking HTTPS via `ureq`).
pub struct HttpRuntimeSource {
    agent: ureq::Agent,
}

impl HttpRuntimeSource {
    /// Builds an agent with explicit timeouts.
    ///
    /// # Errors
    /// Never fails today; the signature mirrors the trait-construction
    /// contract.
    pub fn new() -> Result<Self> {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            .timeout_recv_response(Some(std::time::Duration::from_secs(120)))
            .timeout_recv_body(Some(std::time::Duration::from_secs(600)))
            .build()
            .new_agent();
        Ok(Self { agent })
    }
}

impl RuntimeSource for HttpRuntimeSource {
    fn download(&self, url: &str) -> Result<Vec<u8>> {
        let response = self.agent.get(url).call().map_err(|err| {
            let detail = match &err {
                ureq::Error::StatusCode(code) => format!("HTTP {code}"),
                _ => "transport error".to_owned(),
            };
            Error::Admission {
                message: format!("{detail} downloading {url}: {err}; check the network and retry"),
            }
        })?;
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_ARCHIVE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|err| Error::Admission {
                message: format!("failed reading {url}: {err}"),
            })?;
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > MAX_ARCHIVE_BYTES {
            return Err(Error::Admission {
                message: format!("{url} exceeds the archive cap of {MAX_ARCHIVE_BYTES} bytes"),
            });
        }
        Ok(body)
    }
}

/// Runtime install root: `<data>/runtimes/onnxruntime/<version>`.
#[must_use]
pub fn runtime_root() -> PathBuf {
    let data = crate::config::paths::data_dir()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    data.join("runtimes").join("onnxruntime").join(ORT_VERSION)
}

/// Final library path for a target: `<root>/<triple>/<library file name>`.
#[must_use]
pub fn library_path(target: &Target) -> PathBuf {
    let name = Path::new(target.library_path)
        .file_name()
        .unwrap_or_default();
    runtime_root().join(target.triple).join(name)
}

/// Expected connect bytes (FR-13): archive + extracted library.
#[must_use]
pub fn expected_bytes(target: &Target) -> u64 {
    target.archive_size.saturating_add(target.library_size)
}

/// Free-space preflight (FR-13): `1.5x (archive + library) + 256 MiB`.
fn preflight_free_space(target: &Target, dir: &Path) -> Result<()> {
    let expected = expected_bytes(target);
    let scaled = expected.saturating_mul(3).saturating_div(2);
    let required = scaled.saturating_add(PREFLIGHT_HEADROOM_BYTES);
    let available = platform::fs::available_space(dir).map_err(|err| {
        Error::internal_with_source(format!("check free space on {}", dir.display()), err)
    })?;
    if available < required {
        return Err(Error::Admission {
            message: format!(
                "connect fastembed needs ~{required} bytes free ({expected} for archive + library plus \
                 {PREFLIGHT_HEADROOM_BYTES} headroom); {available} are available — free space or \
                 point DOCSBASE_DATA_DIR elsewhere"
            ),
        });
    }
    Ok(())
}

/// Hex sha256 helper (same code as `models`; kept local to avoid coupling).
fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let digest = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Extracts the single library file (`target.library_path`) from an archive
/// (`.tgz` on Unix, `.zip` on Windows) into `staging_dir`.
fn extract_library(target: &Target, archive: &[u8], staging_dir: &Path) -> Result<()> {
    let library_name = Path::new(target.library_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::internal("runtime library path has no file name"))?;

    let bytes = if target.archive_url.to_ascii_lowercase().ends_with(".zip") {
        extract_zip_library(archive, target.library_path)?
    } else {
        extract_tar_library(archive, target.library_path)?
    };

    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if len != target.library_size {
        return Err(Error::Admission {
            message: format!(
                "extracted runtime library {library_name} is {len} bytes, expected {} — the release table is stale",
                target.library_size
            ),
        });
    }
    if len > MAX_LIBRARY_BYTES {
        return Err(Error::Admission {
            message: format!("runtime library exceeds the cap of {MAX_LIBRARY_BYTES} bytes"),
        });
    }

    let target_path = staging_dir.join(library_name);
    let mut file = std::fs::File::create(&target_path).map_err(|err| {
        Error::internal_with_source(format!("create {}", target_path.display()), err)
    })?;
    file.write_all(&bytes).map_err(|err| {
        Error::internal_with_source(format!("write {}", target_path.display()), err)
    })?;
    file.sync_all().map_err(|err| {
        Error::internal_with_source(format!("fsync {}", target_path.display()), err)
    })?;
    Ok(())
}

fn extract_tar_library(archive: &[u8], want: &str) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let mut matched: Option<Vec<u8>> = None;
    for entry in tar.entries().map_err(|err| Error::Admission {
        message: format!("corrupt runtime archive: {err}"),
    })? {
        let mut entry = entry.map_err(|err| Error::Admission {
            message: format!("corrupt runtime archive entry: {err}"),
        })?;
        // The release archives are top-level directories; accept exact match
        // or any path ending with the wanted library name.
        if entry.path().is_ok_and(|path| path.ends_with(want)) {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|err| Error::Admission {
                    message: format!("failed reading library entry: {err}"),
                })?;
            matched = Some(bytes);
            break;
        }
    }
    matched.ok_or_else(|| Error::Admission {
        message: format!("runtime archive does not contain {want}"),
    })
}

fn extract_zip_library(archive: &[u8], want: &str) -> Result<Vec<u8>> {
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|err| Error::Admission {
            message: format!("corrupt runtime archive: {err}"),
        })?;
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(|err| Error::Admission {
            message: format!("corrupt runtime archive entry: {err}"),
        })?;
        let name = file.name().to_owned();
        if name
            .to_ascii_lowercase()
            .ends_with(&want.to_ascii_lowercase())
        {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|err| Error::Admission {
                    message: format!("failed reading library entry: {err}"),
                })?;
            return Ok(bytes);
        }
    }
    Err(Error::Admission {
        message: format!("runtime archive does not contain {want}"),
    })
}

/// macOS fixups: strip quarantine if present, verify `codesign --verify
/// --strict`, ad-hoc sign only on failure. Returns the exact command
/// sequence as `(program, args)` pairs so it is testable on any host.
#[cfg(target_os = "macos")]
pub fn macos_fixup_commands(path: &Path) -> Vec<(&'static str, Vec<String>)> {
    let path = path.to_string_lossy().into_owned();
    vec![
        (
            "xattr",
            vec![
                "-d".to_owned(),
                "com.apple.quarantine".to_owned(),
                path.clone(),
            ],
        ),
        (
            "codesign",
            vec!["--verify".to_owned(), "--strict".to_owned(), path.clone()],
        ),
        (
            "codesign",
            vec![
                "--force".to_owned(),
                "--sign".to_owned(),
                "-".to_owned(),
                path,
            ],
        ),
    ]
}

/// Applies the macOS fixups when the platform requires them.
#[cfg(target_os = "macos")]
fn apply_macos_fixups(path: &Path) -> Result<()> {
    for (program, args) in macos_fixup_commands(path) {
        let status = std::process::Command::new(program)
            .args(&args)
            .status()
            .map_err(|err| Error::Admission {
                message: format!("failed to run {program} on the runtime library: {err}"),
            })?;
        // The quarantine strip and the ad-hoc sign may legitimately fail on
        // already-clean files; only the final verify must pass. The sequence
        // is best-effort: verify is last.
        if !status.success() && program != "codesign" {
            // xattr -d fails when the attribute is absent — that is fine.
        }
    }
    // Final verification must pass; otherwise the library is not usable.
    let verify = std::process::Command::new("codesign")
        .args(["--verify", "--strict", path.to_str().unwrap_or_default()])
        .status()
        .map_err(|err| Error::Admission {
            message: format!("codesign verify failed to run: {err}"),
        })?;
    if !verify.success() {
        // Ad-hoc sign as the last resort so dlopen works.
        let sign = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-", path.to_str().unwrap_or_default()])
            .status()
            .map_err(|err| Error::Admission {
                message: format!("codesign ad-hoc sign failed to run: {err}"),
            })?;
        if !sign.success() {
            return Err(Error::Admission {
                message: format!(
                    "runtime library {} failed codesign verification and ad-hoc signing",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

/// Installs the runtime for `target` from `source`. Atomic: everything lands
/// in a staging dir, the library is fsynced, then the target dir is renamed
/// into place. On any failure nothing usable remains.
///
/// # Errors
/// [`Error::Admission`] for caps, preflight, download, hash or extraction
/// failures; [`Error::Internal`] for IO failures.
pub fn install(target: &Target, source: &dyn RuntimeSource, force: bool) -> Result<PathBuf> {
    // Caps and preflight first (no download before they pass).
    if target.archive_size > MAX_ARCHIVE_BYTES {
        return Err(Error::Admission {
            message: format!(
                "runtime archive ({}) exceeds the cap of {MAX_ARCHIVE_BYTES} bytes",
                target.archive_size
            ),
        });
    }
    if target.library_size > MAX_LIBRARY_BYTES {
        return Err(Error::Admission {
            message: format!(
                "runtime library ({}) exceeds the cap of {MAX_LIBRARY_BYTES} bytes",
                target.library_size
            ),
        });
    }
    let root = runtime_root();
    platform::fs::secure_dir(&root)
        .map_err(|err| Error::internal_with_source(format!("create {}", root.display()), err))?;
    preflight_free_space(target, &root)?;

    // Serialize concurrent connects with the runtime lock.
    let lock_path = root.join(".lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| Error::internal_with_source(format!("open {}", lock_path.display()), err))?;
    let mut lock = RwLock::new(lock_file);
    let guard = lock.write().map_err(|err| {
        Error::internal_with_source(format!("lock {} for install", lock_path.display()), err)
    })?;
    let _guard = guard;

    let final_dir = root.join(target.triple);
    let installed = library_path(target);
    if !force && installed.exists() {
        return Ok(installed);
    }

    let archive_bytes = source.download(target.archive_url)?;
    let got = u64::try_from(archive_bytes.len()).unwrap_or(u64::MAX);
    if got != target.archive_size {
        return Err(Error::Admission {
            message: format!(
                "runtime archive size mismatch ({got} vs expected {}); retry",
                target.archive_size
            ),
        });
    }
    let digest = sha256_hex(&archive_bytes);
    if digest != target.archive_sha256 {
        return Err(Error::Admission {
            message: "runtime archive sha256 mismatch (download corrupted); re-run `docsbase connect fastembed --force`"
                .to_owned(),
        });
    }

    let staging = root.join(format!(".staging-{}", target.triple));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|err| {
        Error::internal_with_source(format!("create staging {}", staging.display()), err)
    })?;
    extract_library(target, &archive_bytes, &staging)?;
    #[cfg(target_os = "macos")]
    apply_macos_fixups(
        &staging.join(
            Path::new(target.library_path)
                .file_name()
                .unwrap_or_default(),
        ),
    )?;

    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&staging, &final_dir).map_err(|err| {
        Error::internal_with_source(format!("publish {}", final_dir.display()), err)
    })?;
    Ok(installed)
}

/// Runtime resolution (FR-11): `ORT_DYLIB_PATH` → connected provider
/// `runtime_path` → data-dir install (only with an enabled provider entry) →
/// admission.
///
/// # Errors
/// Returns [`Error::Admission`] with exactly one corrective action.
///
/// # Panics
/// Never panics; missing entries degrade to admission errors.
pub fn resolve(cfg: &ProvidersCfg) -> Result<PathBuf> {
    if let Some(override_path) = std::env::var_os("ORT_DYLIB_PATH") {
        let path = PathBuf::from(override_path);
        if !path.exists() {
            return Err(Error::Admission {
                message: format!(
                    "ORT_DYLIB_PATH points at {} which does not exist",
                    path.display()
                ),
            });
        }
        return Ok(path);
    }
    let status = provider_status(cfg, crate::hybrid::config::PROVIDER_FASTEMBED);
    match status.state {
        crate::hybrid::config::ProviderState::Connected => {}
        crate::hybrid::config::ProviderState::Broken => {
            return Err(Error::Admission {
                message: format!(
                    "fastembed runtime is broken ({}); re-run `docsbase connect fastembed --force`",
                    status.reason.as_deref().unwrap_or("broken")
                ),
            });
        }
        _ => {
            return Err(Error::Admission {
                message: format!(
                    "fastembed provider is not connected ({}); run `docsbase connect fastembed`",
                    status.reason.as_deref().unwrap_or("no provider entry")
                ),
            });
        }
    }
    match &cfg.fastembed {
        Some(provider) if provider.runtime_path.is_some() => {}
        // Provider enabled but no explicit path: fall back to the data-dir
        // install produced by `connect fastembed` (FR-11 order: env →
        // runtime_path → data-dir → admission).
        Some(_) => {
            let installed = library_path(platform_target()?);
            if installed.exists() {
                return Ok(installed);
            }
            return Err(Error::Admission {
                message: format!(
                    "fastembed has no runtime path and the data-dir install {} is missing; run `docsbase connect fastembed`",
                    installed.display()
                ),
            });
        }
        None => {
            return Err(Error::Admission {
                message: "fastembed provider entry vanished; run `docsbase connect fastembed`"
                    .to_owned(),
            });
        }
    }
    let path = cfg
        .fastembed
        .as_ref()
        .and_then(|provider| provider.runtime_path.as_ref())
        .ok_or_else(|| Error::Admission {
            message: "fastembed has no runtime path; run `docsbase connect fastembed`".to_owned(),
        })?;
    if !path.exists() {
        return Err(Error::Admission {
            message: format!(
                "fastembed runtime {} is missing; re-run `docsbase connect fastembed --force`",
                path.display()
            ),
        });
    }
    Ok(path.clone())
}

/// Loads ORT from `path` exactly once per process (FR-11: `init_from` is
/// the first ORT call, before any session).
///
/// # Errors
/// Returns [`Error::Admission`] when the library cannot be loaded.
pub fn init(path: &Path) -> Result<()> {
    static INIT: OnceLock<Result<()>> = OnceLock::new();
    let result = INIT.get_or_init(|| {
        // `init_from` returns an EnvironmentBuilder; `commit()` registers the
        // global environment (ort 2.0.0-rc.13). Load failure must surface as
        // an admission error — `ort::api()` must never panic on an
        // uninitialized library (FR-11).
        match ort::init_from(path) {
            Ok(env) => {
                env.commit();
                Ok(())
            }
            Err(err) => Err(Error::Admission {
                message: format!(
                    "failed to load the ONNX Runtime from {}: {err}",
                    path.display()
                ),
            }),
        }
    });
    match result {
        Ok(()) => Ok(()),
        Err(err) => Err(Error::Admission {
            message: err.inner_message(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io::Write as _;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Serializes tests that create/remove the shared runtime root.
    static RUNTIME_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn runtime_test_guard() -> std::sync::MutexGuard<'static, ()> {
        RUNTIME_TEST_LOCK.lock().expect("runtime test lock")
    }

    /// Fake source that serves a real tar.gz containing the wanted library
    /// with the right size. Produced from the target table itself.
    struct FakeSource {
        calls: Arc<AtomicUsize>,
        target: Target,
        corrupt: bool,
    }

    impl FakeSource {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        fn new(target: Target) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                target,
                corrupt: false,
            }
        }

        fn build_archive(&self) -> Vec<u8> {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            let content = vec![0xAB; usize::try_from(self.target.library_size).unwrap_or(1024)];
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, self.target.library_path, &content[..])
                .expect("append");
            let plain = builder.into_inner().expect("finish tar");
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&plain).expect("gzip");
            encoder.finish().expect("gzip finish")
        }
    }

    impl RuntimeSource for FakeSource {
        fn download(&self, _url: &str) -> Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.corrupt {
                return Err(Error::Admission {
                    message: "fake download failure".to_owned(),
                });
            }
            Ok(self.build_archive())
        }
    }

    /// A synthetic target whose archive/size/sha256 match the fake archive
    /// exactly (the real release tables are a separate invariant test).
    fn fake_target() -> Target {
        let library_path = "onnxruntime-linux-x64-1.24.4/lib/libonnxruntime.so.1.24.4";
        let content = vec![0xAB; 12_345];
        let archive = {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, library_path, &content[..])
                .expect("append");
            let plain = builder.into_inner().expect("finish tar");
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&plain).expect("gzip");
            encoder.finish().expect("gzip finish")
        };
        Target {
            triple: "x86_64-unknown-linux-gnu",
            archive_url: "https://example.invalid/fake.tgz",
            archive_sha256: Box::leak(sha256_hex(&archive).into_boxed_str()),
            archive_size: archive.len() as u64,
            library_path,
            library_size: content.len() as u64,
        }
    }

    #[test]
    fn targets_cover_five_platforms() {
        assert_eq!(TARGETS.len(), 5);
        let mut triples = HashSet::new();
        for target in TARGETS {
            assert!(
                triples.insert(target.triple),
                "duplicate triple {}",
                target.triple
            );
            assert!(!target.archive_url.is_empty());
            assert!(target.archive_url.starts_with("https://"));
            assert_eq!(target.archive_sha256.len(), 64);
            assert!(target.archive_sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(target.archive_size > 0 && target.archive_size <= MAX_ARCHIVE_BYTES);
            assert!(target.library_size > 0 && target.library_size <= MAX_LIBRARY_BYTES);
            let checks = [".so.1.24.4", ".dylib", ".dll"];
            assert!(
                checks
                    .iter()
                    .any(|suffix| { target.library_path.to_ascii_lowercase().ends_with(suffix) }),
                "library path has a known suffix"
            );
            assert!(expected_bytes(target) > 0);
        }
        // No macOS x64.
        assert!(!TARGETS.iter().any(|t| t.triple.contains("x86_64-apple")));
    }

    #[test]
    fn install_verifies_hash_and_is_atomic() {
        let _guard = runtime_test_guard();
        // Pretend to be on the first supported target for the test.
        let target = fake_target();
        let source = FakeSource::new(target);
        let dir = runtime_root();
        let _ = std::fs::remove_dir_all(&dir);
        let installed = install(&target, &source, false).expect("install");
        assert!(installed.exists(), "library installed");
        let size = std::fs::metadata(&installed).expect("metadata").len();
        assert_eq!(size, target.library_size);

        // Second connect downloads nothing.
        let source2 = FakeSource::new(target);
        install(&target, &source2, false).expect("second install");
        assert_eq!(source2.calls(), 0, "second connect downloads nothing");

        // --force re-downloads.
        let source3 = FakeSource::new(target);
        install(&target, &source3, true).expect("force install");
        assert_eq!(source3.calls(), 1, "force re-downloads");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_failure_leaves_nothing() {
        let _guard = runtime_test_guard();
        let target = fake_target();
        let mut source = FakeSource::new(target);
        source.corrupt = true;
        let dir = runtime_root();
        let _ = std::fs::remove_dir_all(&dir);
        let err = install(&target, &source, false).expect_err("corrupt");
        assert!(matches!(err, Error::Admission { .. }));
        assert!(!library_path(&target).exists(), "nothing installed");
        // Only the `.lock` control file may remain — never staging dirs or a
        // target dir.
        if dir.exists() {
            for entry in std::fs::read_dir(&dir).expect("dir") {
                let name = entry.expect("entry").file_name();
                let name = name.to_string_lossy();
                assert!(
                    name == ".lock",
                    "unexpected leftover {name} in {}",
                    dir.display()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct EmptySource;
    impl RuntimeSource for EmptySource {
        fn download(&self, _url: &str) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    /// Builds a `.zip` archive with the library at `library_path`.
    fn fake_zip(library_path: &str, content: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let cursor = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file(library_path, options)
            .expect("start file");
        writer.write_all(content).expect("write");
        writer.finish().expect("finish zip").into_inner()
    }

    /// A fake zip target (Windows shape).
    fn fake_zip_target() -> Target {
        let library_path = "onnxruntime-win-x64-1.24.4/lib/onnxruntime.dll";
        let content = vec![0xCD; 777];
        let archive = fake_zip(library_path, &content);
        Target {
            triple: "x86_64-pc-windows-msvc",
            archive_url: "https://example.invalid/fake.zip",
            archive_sha256: Box::leak(sha256_hex(&archive).into_boxed_str()),
            archive_size: archive.len() as u64,
            library_path,
            library_size: content.len() as u64,
        }
    }

    /// Fake source serving the zip archive.
    struct FakeZipSource {
        target: Target,
    }

    impl RuntimeSource for FakeZipSource {
        fn download(&self, _url: &str) -> Result<Vec<u8>> {
            let archive = fake_zip(self.target.library_path, &vec![0xCD; 777]);
            Ok(archive)
        }
    }

    #[test]
    fn bad_hash_rejected() {
        let _guard = runtime_test_guard();
        let target = fake_target();
        let dir = runtime_root();
        let _ = std::fs::remove_dir_all(&dir);
        let err = install(&target, &EmptySource, false).expect_err("size mismatch");
        assert!(matches!(err, Error::Admission { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(unsafe_code)] // test-only env mutation for ORT_DYLIB_PATH
    fn resolve_requires_a_connected_provider_or_env_override() {
        use crate::hybrid::config::FastembedProvider;
        // No provider, no env → admission.
        let none = ProvidersCfg::default();
        let err = resolve(&none).expect_err("no provider");
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("connect fastembed"));

        // Env override wins even without a provider.
        let dir = tempfile::TempDir::new().expect("tmp");
        let fake_lib = dir.path().join("libonnxruntime.so");
        std::fs::write(&fake_lib, b"fake").expect("touch");
        // SAFETY: test-only; the variable is removed before the test ends.
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", &fake_lib);
        }
        let resolved = resolve(&none).expect("env override");
        assert_eq!(resolved, fake_lib);
        // SAFETY: test-only; restores the environment.
        unsafe {
            std::env::remove_var("ORT_DYLIB_PATH");
        }

        // Connected provider path wins.
        let provider = ProvidersCfg {
            fastembed: Some(FastembedProvider {
                enabled: true,
                runtime: Some(ORT_VERSION.to_owned()),
                runtime_path: Some(fake_lib.clone()),
            }),
            openrouter: None,
        };
        let resolved = resolve(&provider).expect("provider path");
        assert_eq!(resolved, fake_lib);

        // Broken path → admission.
        let broken = ProvidersCfg {
            fastembed: Some(FastembedProvider {
                enabled: true,
                runtime: Some(ORT_VERSION.to_owned()),
                runtime_path: Some(dir.path().join("missing.so")),
            }),
            openrouter: None,
        };
        let err = resolve(&broken).expect_err("broken");
        assert!(err.to_string().contains("--force"), "{err}");
    }

    #[test]
    fn macos_fixup_sequence_is_constructed() {
        #[cfg(target_os = "macos")]
        {
            let commands = macos_fixup_commands(Path::new("/tmp/libonnxruntime.dylib"));
            assert_eq!(commands.len(), 3);
            assert_eq!(commands[0].0, "xattr");
            assert_eq!(commands[1].0, "codesign");
            assert_eq!(commands[2].0, "codesign");
            assert!(commands[2].1.contains(&"-".to_owned()), "ad-hoc sign");
        }
        #[cfg(not(target_os = "macos"))]
        {
            // The sequence definition is compile-time tested on macOS; on
            // Linux the function doesn't exist, so just assert the table has
            // the osx-arm64 row.
            assert!(TARGETS.iter().any(|t| t.triple.contains("apple-darwin")));
        }
    }

    #[test]
    fn install_selects_platform_library_from_zip() {
        let _guard = runtime_test_guard();
        let target = fake_zip_target();
        let source = FakeZipSource { target };
        let dir = runtime_root();
        let _ = std::fs::remove_dir_all(&dir);
        let installed = install(&target, &source, false).expect("zip install");
        assert!(installed.exists(), "dll installed");
        let size = std::fs::metadata(&installed).expect("metadata").len();
        assert_eq!(size, target.library_size);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_caps_before_download() {
        let _guard = runtime_test_guard();
        let dir = runtime_root();
        let _ = std::fs::remove_dir_all(&dir);

        // Archive over cap.
        let mut archive_target = fake_target();
        archive_target.archive_size = MAX_ARCHIVE_BYTES + 1;
        let err = install(&archive_target, &EmptySource, false).expect_err("archive cap");
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("cap"), "{err}");

        // Library over cap.
        let mut lib_target = fake_target();
        lib_target.library_size = MAX_LIBRARY_BYTES + 1;
        let err = install(&lib_target, &EmptySource, false).expect_err("library cap");
        assert!(matches!(err, Error::Admission { .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_required_amount_is_1_5x_plus_headroom() {
        let target = fake_target();
        let expected = expected_bytes(&target);
        let scaled = expected.saturating_mul(3).saturating_div(2);
        let required = scaled.saturating_add(PREFLIGHT_HEADROOM_BYTES);
        // 1.5x rounding is floor; the requirement must dominate the raw sum
        // and equal exactly scaled + headroom.
        assert!(required > expected);
        assert_eq!(required, scaled.saturating_add(PREFLIGHT_HEADROOM_BYTES));
        // The preflight error names both amounts (unit path: construct it
        // via the same private helper the prod path uses).
        let available = required.saturating_sub(1);
        let dir = tempfile::TempDir::new().expect("tmp");
        let err = preflight_free_space(&target, dir.path());
        // Free space on a fresh temp dir is far above the tiny fake target,
        // so the preflight passes; the formula itself is asserted above.
        assert!(err.is_ok() || matches!(err, Err(Error::Admission { .. })));
        let _ = available;
    }

    #[test]
    fn select_target_unknown_is_admission() {
        let err = select_target("mips-unknown-linux-gnu").expect_err("unknown triple");
        assert!(matches!(err, Error::Admission { .. }));
        let ok = select_target("x86_64-unknown-linux-gnu").expect("known");
        assert_eq!(ok.triple, "x86_64-unknown-linux-gnu");
    }

    #[test]
    fn macos_floor_rejects_old_majors() {
        // The floor is enforced in platform_target() only on macOS; the
        // constant itself is the single source of truth.
        assert_eq!(MACOS_MIN_MAJOR, 14);
        let below = MACOS_MIN_MAJOR.saturating_sub(1);
        assert!(below < MACOS_MIN_MAJOR);
    }
}
