//! `docsbase update` — fetch a release and replace the running binary (FR-5).
//!
//! This is the only network-touching code path in the binary; like the install
//! scripts it shells out to the platform tooling they already require
//! (`curl`, `sha256sum`, `tar` on Linux; PowerShell on Windows), so the crate
//! gains no HTTP stack and the daemon stays strictly local. The daemon
//! coordination (stop, admission lease, owned manifest) is shared with
//! `cli::install`. The `.sha256` from the release is checked so a truncated
//! download never reaches the swap; it is an integrity check, not a security
//! boundary (see `docs/INSTALL.md`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;

use crate::cli::install;

/// Owner/repo used when `DOCSBASE_REPO` is unset.
const DEFAULT_REPO: &str = "punkhomov/docsbase-memory-mcp";
const API_BASE: &str = "https://api.github.com";
const RELEASES_BASE: &str = "https://github.com";

/// Flags for [`run`].
#[derive(Debug)]
pub struct Options {
    /// Only report whether a newer release exists.
    pub check: bool,
    /// Reinstall even when the running version already matches the target.
    pub force: bool,
    /// Explicit release tag to install instead of `releases/latest`.
    pub version: Option<String>,
    /// Install a local binary, skipping download and verification.
    pub from: Option<PathBuf>,
}

/// Runs `docsbase update`.
///
/// # Errors
/// Fails when a release cannot be resolved, downloaded, verified or extracted,
/// or when the daemon coordination in [`install::install_from`] refuses.
pub fn run(opts: &Options) -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    if let Some(from) = opts.from.as_deref() {
        return install_local(current, from);
    }

    let repo = repo_from_env()?;
    let explicit = opts.version.is_some();
    let tag = match opts.version.as_deref() {
        Some(raw) => normalize_tag(raw)?,
        None => latest_tag(&repo)?,
    };
    let version = version_of(&tag);
    if opts.check {
        if version == current {
            println!("docsbase {current} is up to date (latest {tag})");
        } else {
            println!("update available: {current} -> {version} ({tag})");
        }
        return Ok(());
    }
    if version == current && !opts.force && !explicit {
        println!(
            "docsbase {current} is already up to date ({tag}); re-run with --force to reinstall"
        );
        return Ok(());
    }

    let tmp = TempDir::new()?;
    let asset = asset_name(version)?;
    let archive = tmp.path().join(&asset);
    let base = format!("{RELEASES_BASE}/{repo}/releases/download/{tag}");
    download(&format!("{base}/{asset}"), &archive)?;
    let checksum = tmp.path().join(format!("{asset}.sha256"));
    download(&format!("{base}/{asset}.sha256"), &checksum)?;
    verify_sha256(&archive, &checksum)?;
    let newbin = extract(&archive, tmp.path())?;

    let (target, data) =
        install::install_from(&newbin).with_context(|| format!("install {}", newbin.display()))?;
    self_replace(&target);
    println!("updated docsbase {current} -> {version}");
    println!(
        "installed {} ({})",
        target.display(),
        crate::ipc::protocol::build_id()
    );
    println!("manifest: {}", data.join(install::MANIFEST).display());
    println!("the daemon was stopped; it restarts on next use");
    Ok(())
}

fn install_local(current: &str, from: &Path) -> anyhow::Result<()> {
    let (target, data) =
        install::install_from(from).with_context(|| format!("install {}", from.display()))?;
    self_replace(&target);
    println!("updated docsbase {current} -> {}", from.display());
    println!(
        "installed {} ({})",
        target.display(),
        crate::ipc::protocol::build_id()
    );
    println!("manifest: {}", data.join(install::MANIFEST).display());
    Ok(())
}

/// Replaces the binary that invoked this process when it is a different copy
/// from the owned one (install scripts keep one in `~/.local/bin`, cargo in
/// `~/.cargo/bin`), so the next invocation runs the new build. A failed
/// replacement only warns: the owned copy is already updated.
fn self_replace(owned: &Path) {
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    if same_path(&current, owned) {
        return;
    }
    if let Err(err) = install::swap_binary(owned, &current) {
        eprintln!(
            "warning: could not replace {} ({err}); re-run the installer or put {} on PATH",
            current.display(),
            owned.display()
        );
        return;
    }
    println!("updated invoked binary {}", current.display());
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Resolves the latest release tag for `repo`.
/// Resolves the latest release tag for `repo`. Prefers the stable `latest`
/// release; repos that only publish prereleases have none, so the newest
/// published release is the fallback.
fn latest_tag(repo: &str) -> anyhow::Result<String> {
    let stable = format!("{API_BASE}/repos/{repo}/releases/latest");
    if let Ok(json) = fetch_json(&stable)
        && let Some(tag) = json["tag_name"].as_str()
    {
        return normalize_tag(tag);
    }
    let list = fetch_json(&format!("{API_BASE}/repos/{repo}/releases?per_page=1"))?;
    let tag = list
        .as_array()
        .and_then(|releases| releases.first())
        .and_then(|release| release["tag_name"].as_str())
        .context("no published release found")?;
    normalize_tag(tag)
}

fn fetch_json(url: &str) -> anyhow::Result<serde_json::Value> {
    let tmp = TempDir::new()?;
    let path = tmp.path().join("response.json");
    download(url, &path)?;
    let bytes = fs::read(&path).context("read response")?;
    serde_json::from_slice(&bytes).context("parse response")
}

fn repo_from_env() -> anyhow::Result<String> {
    let repo = std::env::var("DOCSBASE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_owned());
    parse_repo(&repo)
}

fn parse_repo(repo: &str) -> anyhow::Result<String> {
    let Some((owner, name)) = repo.split_once('/') else {
        anyhow::bail!("DOCSBASE_REPO must be owner/name, got {repo:?}");
    };
    if valid_segment(owner) && valid_segment(name) {
        Ok(repo.to_owned())
    } else {
        anyhow::bail!("DOCSBASE_REPO must be owner/name, got {repo:?}");
    }
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 100
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Accepts `X.Y.Z` or `vX.Y.Z` and rejects anything that could escape the
/// release URL when interpolated.
fn normalize_tag(raw: &str) -> anyhow::Result<String> {
    let raw = raw.trim();
    let valid = !raw.is_empty()
        && raw.len() <= 128
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    if !valid {
        anyhow::bail!("invalid release tag {raw:?} (expected vX.Y.Z)");
    }
    Ok(if raw.starts_with('v') {
        raw.to_owned()
    } else {
        format!("v{raw}")
    })
}

fn version_of(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Release asset name, mirroring the packaging in `.github/workflows/release.yml`.
fn asset_name(version: &str) -> anyhow::Result<String> {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        anyhow::bail!("unsupported OS: prebuilt releases cover Linux and Windows only");
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        anyhow::bail!(
            "unsupported architecture {}: prebuilt releases cover x86_64 only",
            std::env::consts::ARCH
        );
    };
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    Ok(format!("docsbase-{version}-{os}-{arch}.{ext}"))
}

fn verify_sha256(archive: &Path, checksum_file: &Path) -> anyhow::Result<()> {
    let text = fs::read_to_string(checksum_file)
        .with_context(|| format!("read {}", checksum_file.display()))?;
    let expected = parse_hash(&text)
        .with_context(|| format!("{} is not a sha256 file", checksum_file.display()))?;
    let actual = sha256_file(archive)?;
    if actual != expected {
        anyhow::bail!(
            "sha256 mismatch for {}: expected {expected}, got {actual}",
            archive.display()
        );
    }
    Ok(())
}

/// First whitespace-delimited token, accepting the `sha256sum` binary marker.
fn parse_hash(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    let token = token.trim_start_matches('*');
    (token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| token.to_ascii_lowercase())
}

/// Downloads `url` to `dest` with the platform tooling (`curl` / PowerShell).
fn download(url: &str, dest: &Path) -> anyhow::Result<()> {
    if cfg!(windows) {
        let script = format!(
            "$ProgressPreference='SilentlyContinue'; \
             Invoke-WebRequest -UseBasicParsing -Uri '{uri}' -OutFile '{out}'",
            uri = ps_literal(url),
            out = ps_literal(&dest.to_string_lossy()),
        );
        run_tool(&powershell(&script))?;
    } else {
        let dest_str = dest.to_string_lossy();
        run_tool(&[
            "curl",
            "-fsSL",
            "--proto",
            "=https",
            "-o",
            dest_str.as_ref(),
            url,
        ])?;
    }
    Ok(())
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let stdout = if cfg!(windows) {
        let script = format!(
            "(Get-FileHash -Algorithm SHA256 -LiteralPath '{}').Hash",
            ps_literal(&path.to_string_lossy())
        );
        let output = run_tool(&powershell(&script))?;
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        let path_str = path.to_string_lossy();
        let output = run_tool(&["sha256sum", path_str.as_ref()])?;
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    parse_hash(&stdout).with_context(|| format!("cannot parse sha256 of {}", path.display()))
}

/// Extracts `archive` into `dir` and returns the `docsbase` binary inside.
fn extract(archive: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    if cfg!(windows) {
        let script = format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            ps_literal(&archive.to_string_lossy()),
            ps_literal(&dir.to_string_lossy()),
        );
        run_tool(&powershell(&script))?;
    } else {
        let archive_str = archive.to_string_lossy();
        let dir_str = dir.to_string_lossy();
        run_tool(&["tar", "-xzf", archive_str.as_ref(), "-C", dir_str.as_ref()])?;
    }
    let binary = dir.join(format!("docsbase{}", std::env::consts::EXE_SUFFIX));
    if !binary.is_file() {
        anyhow::bail!(
            "{} did not contain {}",
            archive.display(),
            binary.file_name().map_or_else(
                || "docsbase".to_owned(),
                |name| name.to_string_lossy().into_owned()
            )
        );
    }
    Ok(binary)
}

/// Runs a command, turning a missing tool or a non-zero exit into an error.
fn run_tool(args: &[&str]) -> anyhow::Result<std::process::Output> {
    let (program, rest) = args.split_first().context("run: empty command line")?;
    let output = Command::new(*program)
        .args(rest.iter().copied())
        .output()
        .with_context(|| format!("run {program} (is it installed and on PATH?)"))?;
    if !output.status.success() {
        anyhow::bail!(
            "{program} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

fn powershell(script: &str) -> Vec<&str> {
    vec![
        "powershell",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]
}

/// Escapes a single-quoted PowerShell literal.
fn ps_literal(value: &str) -> String {
    value.replace('\'', "''")
}

/// Scratch directory removed on drop (the process aborts on panic, leaving
/// stale entries to the OS temp cleaner).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> anyhow::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path =
            std::env::temp_dir().join(format!("docsbase-update-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_matches_release_convention() {
        let name = asset_name("0.1.0-alpha.1").expect("asset name");
        if cfg!(windows) {
            assert_eq!(name, "docsbase-0.1.0-alpha.1-windows-x86_64.zip");
        } else {
            assert_eq!(name, "docsbase-0.1.0-alpha.1-linux-x86_64.tar.gz");
        }
    }

    #[test]
    fn tags_normalize_and_reject_injection() {
        assert_eq!(normalize_tag("v1.2.3").expect("tag"), "v1.2.3");
        assert_eq!(normalize_tag("1.2.3").expect("tag"), "v1.2.3");
        assert_eq!(version_of("v1.2.3"), "1.2.3");
        assert!(normalize_tag("v1.2.3; rm -rf /").is_err());
        assert!(normalize_tag("v1.2.3/../../x").is_err());
        assert!(normalize_tag("").is_err());
    }

    #[test]
    fn checksum_parsing_accepts_sha256sum_forms() {
        let hash = "a".repeat(64);
        assert_eq!(
            parse_hash(&format!("{hash}  docsbase.tar.gz")),
            Some(hash.clone())
        );
        assert_eq!(
            parse_hash(&format!("{hash} *docsbase.tar.gz")),
            Some(hash.clone())
        );
        assert_eq!(parse_hash("not-a-hash  file"), None);
        assert_eq!(parse_hash(""), None);
    }

    #[test]
    fn repo_requires_owner_and_name() {
        assert!(parse_repo("owner/repo").is_ok());
        assert!(parse_repo("owner/repo/extra").is_err());
        assert!(parse_repo("owner").is_err());
        assert!(parse_repo("own er/repo").is_err());
    }

    #[test]
    fn powershell_literals_escape_quotes() {
        assert_eq!(ps_literal("C:\\temp\\it's"), "C:\\temp\\it''s");
    }
}
