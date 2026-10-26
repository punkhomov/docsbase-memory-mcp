//! Local model registry and cache for the `hybrid-local` provider (ADR-17).
//!
//! Registry rows are constants in this module: names, tasks, pinned HF
//! revisions, per-file sizes and sha256, pooling, prefixes. The registry
//! selects nothing; selection happens via `docsbase models use` (T6).
//!
//! The cache lives at `<cache>/models/<name>/<revision>/`; `.complete` is
//! written last (FR-6), a `.lock` file serializes installs against removal,
//! and every cached file is re-hashed before it is trusted (NFR-3). This
//! module is compiled only under `hybrid-local`, so the default build links
//! none of it (NFR-1).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use fd_lock::RwLock;
use sha2::Digest;

use crate::error::{Error, Result};
use crate::platform;

/// Per-file size cap (NFR-3): a single model file over 2 GiB is refused.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Whole-model size cap (NFR-3): a model over 3 GiB in total is refused.
pub const MAX_TOTAL_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// Free-space headroom added to the fetch preflight (FR-13).
pub const PREFLIGHT_HEADROOM_BYTES: u64 = 256 * 1024 * 1024;

/// Bounded retries for the exclusive install lock (round-2 P1b): an
/// embedder/reranker holds the revision `.lock` shared for its process
/// lifetime, so a plain blocking `write()` would hang the admin CLI
/// forever; mirror `with_config_write_lock`'s bounded try-retry.
const EXCLUSIVE_LOCK_RETRIES: u32 = 50;
const EXCLUSIVE_LOCK_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

/// Acquires the exclusive install lock with bounded try-retries. Returns
/// `None` when the budget is exhausted (a loaded instance holds the lock
/// shared for its lifetime). Each attempt uses a fresh leaked `'static`
/// lock (same pattern as `take_shared_lock`/`remove_cmd`); failed attempts
/// leak one fd+Box each — bounded and deliberate.
fn acquire_exclusive_lock(
    dir: &Path,
) -> Result<Option<fd_lock::RwLockWriteGuard<'static, std::fs::File>>> {
    for _ in 0..EXCLUSIVE_LOCK_RETRIES {
        let file = open_lock(dir)?;
        let lock: &'static mut RwLock<std::fs::File> = Box::leak(Box::new(RwLock::new(file)));
        if let Ok(guard) = lock.try_write() {
            return Ok(Some(guard));
        }
        std::thread::sleep(EXCLUSIVE_LOCK_RETRY_DELAY);
    }
    Ok(None)
}

/// Serial number for unique staging/backup dir names (see `fetch_into`).
static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// Preflight requirement (FR-13): 1.5x the expected bytes plus headroom,
/// expressed as the integer fraction `3/2` (floor equals the float
/// truncation for non-negative values).
pub const PREFLIGHT_MULTIPLIER_NUMERATOR: u64 = 3;
pub const PREFLIGHT_MULTIPLIER_DENOMINATOR: u64 = 2;

/// What a registry model is used for (data model §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Text embedding (vector output).
    Embedding,
    /// Cross-encoder reranking (score output).
    Rerank,
}

/// Pooling strategy for an embedding model (fastembed `Pooling`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// Take the CLS token's hidden state.
    Cls,
    /// Mean-pool the token hidden states.
    Mean,
}

/// One pinned file of a registry model. `path` is relative to the model's
/// cache directory; the ONNX file and the four fastembed tokenizer files are
/// all listed here — this list is authoritative (design §5).
#[derive(Debug, Clone, Copy)]
pub struct FileSpec {
    /// Path inside the model cache directory.
    pub path: &'static str,
    /// Exact file size in bytes.
    pub size: u64,
    /// Hex sha256 of the file content.
    pub sha256: &'static str,
}

/// A registry row (data model §7). All fields are constants; fetching and
/// verification read only from `files`.
#[derive(Debug, Clone, Copy)]
pub struct ModelEntry<'a> {
    /// Registry name used by `models fetch` / `models use`.
    pub name: &'a str,
    /// Embedding or rerank.
    pub task: Task,
    /// Hugging Face repo (`<org>/<model>`).
    pub repo: &'a str,
    /// Pinned commit revision (full sha).
    pub revision: &'a str,
    /// Embedding width; `None` for rerank rows.
    pub dim: Option<usize>,
    /// Embedding pooling; `None` for rerank rows.
    pub pooling: Option<Pooling>,
    /// Quantization (e.g. `int8`); `None` for fp32 rows.
    pub quantization: Option<&'a str>,
    /// Output tensor key; `None` uses the fastembed default precedence.
    pub output_key: Option<&'a str>,
    /// License of the model weights.
    pub license: &'a str,
    /// Prefix prepended to queries (FR-10). Empty for models with no query
    /// instruction (e.g. rerank rows).
    pub query_prefix: &'a str,
    /// Prefix prepended to passages (FR-10). Empty when none.
    pub passage_prefix: &'a str,
    /// Prompt version; bumping it invalidates cached vectors (FR-9).
    pub prompt_version: u32,
    /// Authoritative file set: ONNX + tokenizer/config files.
    pub files: &'a [FileSpec],
}

impl ModelEntry<'_> {
    /// Sum of the file sizes, used by the preflight (FR-13).
    #[must_use]
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// First eight characters of the pinned revision (cache-key component).
    #[must_use]
    pub fn revision_short(&self) -> String {
        self.revision.chars().take(8).collect()
    }
}

/// The closed registry (FR-5). Nothing is selected by default; `models use`
/// is the only selector. Sizes/sha256 come from the pinned HF revisions
/// (tree API), verified during spec work (progress.md, T1).
pub const REGISTRY: &[ModelEntry<'static>] = &[
    ModelEntry {
        name: "bge-small-en-v1.5",
        task: Task::Embedding,
        repo: "Xenova/bge-small-en-v1.5",
        revision: "ea104dacec62c0de699686887e3f920caeb4f3e3",
        dim: Some(384),
        pooling: Some(Pooling::Cls),
        quantization: None,
        output_key: Some("last_hidden_state"),
        license: "MIT",
        query_prefix: "Represent this sentence for searching relevant passages: ",
        passage_prefix: "",
        prompt_version: 1,
        files: &[
            FileSpec {
                path: "onnx/model.onnx",
                size: 133_093_490,
                sha256: "828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35",
            },
            FileSpec {
                path: "tokenizer.json",
                size: 711_396,
                sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
            },
            FileSpec {
                path: "config.json",
                size: 683,
                sha256: "fa73f90bf92c8cace1fbcb709626306f2bdbc9ea3e5b5f94b440df9b6aa56350",
            },
            FileSpec {
                path: "special_tokens_map.json",
                size: 125,
                sha256: "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3",
            },
            FileSpec {
                path: "tokenizer_config.json",
                size: 366,
                sha256: "9261e7d79b44c8195c1cada2b453e55b00aeb81e907a6664974b4d7776172ab3",
            },
        ],
    },
    ModelEntry {
        name: "multilingual-e5-small",
        task: Task::Embedding,
        repo: "Xenova/multilingual-e5-small",
        revision: "761b726dd34fb83930e26aab4e9ac3899aa1fa78",
        dim: Some(384),
        pooling: Some(Pooling::Mean),
        quantization: Some("int8"),
        output_key: Some("last_hidden_state"),
        license: "MIT",
        query_prefix: "query: ",
        passage_prefix: "passage: ",
        prompt_version: 1,
        files: &[
            FileSpec {
                path: "onnx/model_int8.onnx",
                size: 118_054_593,
                sha256: "4d24e2bc01a447951524466ef533e52944bf48509e6552810bcee1a2711cb02c",
            },
            FileSpec {
                path: "tokenizer.json",
                size: 17_082_730,
                sha256: "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
            },
            FileSpec {
                path: "config.json",
                size: 658,
                sha256: "cb99455288675345e1a4f411438d5d0adbba5fbd3a67ea4fb03c015433b996c1",
            },
            FileSpec {
                path: "special_tokens_map.json",
                size: 167,
                sha256: "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
            },
            FileSpec {
                path: "tokenizer_config.json",
                size: 443,
                sha256: "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
            },
        ],
    },
    ModelEntry {
        name: "jina-reranker-v1-turbo-en",
        task: Task::Rerank,
        repo: "jinaai/jina-reranker-v1-turbo-en",
        revision: "b8c14f4e723d9e0aab4732a7b7b93741eeeb77c2",
        dim: None,
        pooling: None,
        quantization: Some("int8"),
        output_key: None,
        license: "Apache-2.0",
        query_prefix: "",
        passage_prefix: "",
        prompt_version: 1,
        files: &[
            FileSpec {
                path: "onnx/model_int8.onnx",
                size: 38_295_104,
                sha256: "3defdef1ae34e119bd704216087743e79665934c96aebabcb6077c239dc3ae66",
            },
            FileSpec {
                path: "tokenizer.json",
                size: 2_030_772,
                sha256: "0046da43cc8c424b317f56b092b0512aaaa65c4f925d2f16af9d9eeb4d0ef902",
            },
            FileSpec {
                path: "config.json",
                size: 1_206,
                sha256: "e050ff6a15ae9295e84882fa0e98051bd8754856cd5201395ebf00ce9f2d609b",
            },
            FileSpec {
                path: "special_tokens_map.json",
                size: 280,
                sha256: "06e405a36dfe4b9604f484f6a1e619af1a7f7d09e34a8555eb0b77b66318067f",
            },
            FileSpec {
                path: "tokenizer_config.json",
                size: 1_215,
                sha256: "d291c6652d96d56ffdbcf1ea19d9bae5ed79003f7648c627e725a619227ce8fa",
            },
        ],
    },
    ModelEntry {
        name: "bge-reranker-base",
        task: Task::Rerank,
        repo: "BAAI/bge-reranker-base",
        revision: "2cfc18c9415c912f9d8155881c133215df768a70",
        dim: None,
        pooling: None,
        quantization: None,
        output_key: None,
        license: "MIT",
        query_prefix: "",
        passage_prefix: "",
        prompt_version: 1,
        files: &[
            FileSpec {
                path: "onnx/model.onnx",
                size: 1_112_459_588,
                sha256: "15b9a8c3da82eddf263df571281166e00e9308fe19d077084b642ebfcaf06d2b",
            },
            FileSpec {
                path: "tokenizer.json",
                size: 17_098_107,
                sha256: "9eb652ac4e40cc093272bbbe0f55d521cf67570060227109b5cdc20945a4489e",
            },
            FileSpec {
                path: "config.json",
                size: 799,
                sha256: "289adf7ada1eb6b4afa7589a48a032d45a076cf2e46dcdb3b4cabc33be14f708",
            },
            FileSpec {
                path: "special_tokens_map.json",
                size: 279,
                sha256: "d5469a60db23249c7f8945013d78df30b44b6bf686c6bb4740f4223f77b1b535",
            },
            FileSpec {
                path: "tokenizer_config.json",
                size: 443,
                sha256: "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
            },
        ],
    },
];

/// Looks up a registry entry by name (FR-5).
///
/// # Errors
/// Returns [`Error::Admission`] for an unknown name (the registry is closed).
pub fn entry(name: &str) -> Result<&'static ModelEntry<'static>> {
    REGISTRY
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| Error::Admission {
            message: format!(
                "unknown model \"{name}\"; `docsbase models list` shows the registry entries"
            ),
        })
}

/// Cache directory for one model revision: `<cache>/models/<name>/<revision>`.
///
/// Falls back to the current directory when XDG discovery fails so the
/// function stays infallible; callers see the failure only on the first
/// filesystem operation.
#[must_use]
pub fn cache_dir(name: &str, revision: &str) -> PathBuf {
    let root = crate::config::paths::cache_dir()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    root.join("models").join(name).join(revision)
}

/// Canonical model id (FR-9): `fastembed/<name>@d<dim>@<l2|raw>@r<rev8>@p<prompt_version>`.
/// This is the single formatter for the fastembed arm; the embedder,
/// `coverage()` and the ANN warm path all share it (SC-10).
#[must_use]
pub fn canonical_id(entry: &ModelEntry<'_>, dims: usize, normalize: bool) -> String {
    let post = if normalize { "l2" } else { "raw" };
    format!(
        "fastembed/{name}@d{dims}@{post}@r{rev8}@p{prompt_version}",
        name = entry.name,
        rev8 = entry.revision_short(),
        prompt_version = entry.prompt_version
    )
}

/// Canonical id for a registry name (FR-9); `None` for an unknown name. This
/// is the lookup used by [`crate::hybrid::config::embedding_model_id`] so
/// the formatter lives here exactly once.
#[must_use]
pub fn canonical_id_for(name: &str, dims: usize, normalize: bool) -> Option<String> {
    entry(name)
        .ok()
        .map(|entry| canonical_id(entry, dims, normalize))
}

/// A source of model file bytes. Production uses [`HttpModelSource`]; tests
/// use a fake. `path` is the registry-relative file path.
pub trait ModelSource {
    /// Downloads one file and returns its bytes (small-file/test path).
    ///
    /// # Errors
    /// Returns [`Error::Admission`] for transport/hash-class failures; the
    /// caller applies the per-file cap.
    fn fetch(&self, repo: &str, revision: &str, path: &str) -> Result<Vec<u8>>;

    /// Streams one file into `sink` (large-file path; round-3
    /// mimo-4/muse-1: no whole-file RAM buffer). The default materializes
    /// via [`fetch`]; [`HttpModelSource`] overrides this to stream with an
    /// in-flight per-file cap.
    ///
    /// # Errors
    /// Same as [`fetch`]; sink write failures are [`Error::Admission`].
    fn fetch_to(
        &self,
        repo: &str,
        revision: &str,
        path: &str,
        sink: &mut dyn std::io::Write,
    ) -> Result<()> {
        let bytes = self.fetch(repo, revision, path)?;
        sink.write_all(&bytes).map_err(|err| Error::Admission {
            message: format!("write download for {path}: {err}"),
        })
    }
}

/// The production source: blocking HTTPS via `ureq` (rustls). Used only by
/// `docsbase models fetch`; search/index never touch it (FR-12).
pub struct HttpModelSource {
    agent: ureq::Agent,
}

impl HttpModelSource {
    /// Builds an agent with explicit timeouts.
    ///
    /// # Errors
    /// Never fails today; the signature mirrors the trait-construction
    /// contract.
    pub fn new() -> Result<Self> {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            .timeout_recv_response(Some(std::time::Duration::from_secs(120)))
            .timeout_recv_body(Some(std::time::Duration::from_secs(300)))
            .build()
            .new_agent();
        Ok(Self { agent })
    }
}

impl ModelSource for HttpModelSource {
    fn fetch(&self, repo: &str, revision: &str, path: &str) -> Result<Vec<u8>> {
        let url = format!("https://huggingface.co/{repo}/resolve/{revision}/{path}");
        let response = self.agent.get(&url).call().map_err(|err| {
            let detail = match &err {
                ureq::Error::StatusCode(code) => format!("HTTP {code}"),
                _ => "transport error".to_owned(),
            };
            Error::Admission {
                message: format!("{detail} fetching {url}: {err}; check the network and retry"),
            }
        })?;
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|err| Error::Admission {
                message: format!("failed reading {url}: {err}"),
            })?;
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
            return Err(Error::Admission {
                message: format!("{url} exceeds the per-file cap of {MAX_FILE_BYTES} bytes"),
            });
        }
        Ok(body)
    }

    fn fetch_to(
        &self,
        repo: &str,
        revision: &str,
        path: &str,
        sink: &mut dyn std::io::Write,
    ) -> Result<()> {
        let url = format!("https://huggingface.co/{repo}/resolve/{revision}/{path}");
        let response = self.agent.get(&url).call().map_err(|err| {
            let detail = match &err {
                ureq::Error::StatusCode(code) => format!("HTTP {code}"),
                _ => "transport error".to_owned(),
            };
            Error::Admission {
                message: format!("{detail} fetching {url}: {err}; check the network and retry"),
            }
        })?;
        let mut reader = response.into_body().into_reader().take(MAX_FILE_BYTES + 1);
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut total: u64 = 0;
        loop {
            let read = reader.read(&mut buffer).map_err(|err| Error::Admission {
                message: format!("failed reading {url}: {err}"),
            })?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > MAX_FILE_BYTES {
                return Err(Error::Admission {
                    message: format!("{url} exceeds the per-file cap of {MAX_FILE_BYTES} bytes"),
                });
            }
            sink.write_all(&buffer[..read])
                .map_err(|err| Error::Admission {
                    message: format!("write download for {url}: {err}"),
                })?;
        }
        Ok(())
    }
}

/// Opens (creating if needed) the `.lock` file of a model cache directory.
/// Opens the model-root lock file `<model_root>/.lock` for `dir` (a
/// revision dir). The lock lives at the model root, NOT inside the revision
/// dir: a successful promote renames the revision dir away (C6), so an
/// in-dir lock would be unlinked and a new inode would replace it — loaded
/// instances would hold a guard on a dead inode and `models remove`/fetch
/// would probe a fresh uncontended one (round-2 D2/D3). A root-level lock
/// is stable across promotes and is held exclusively for the whole install
/// (staging + promote; staging/backup live as siblings under `<cache>/models`,
/// round-3 deepseek-4), so a concurrent remove is refused while a fetch is
/// in flight.
fn open_lock(dir: &Path) -> Result<std::fs::File> {
    let root = dir
        .parent()
        .ok_or_else(|| Error::internal(format!("cache path has no parent: {}", dir.display())))?;
    let path = root.join(".lock");
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| Error::internal_with_source(format!("open {}", path.display()), err))
}

/// Full re-verification of a cached revision: every registry file must exist
/// with the exact size and sha256 (FR-6, NFR-3). Returns `false` (not an
/// error) when a file is missing or mismatched; IO failures are errors.
fn verify_hashes(entry: &ModelEntry<'_>, dir: &Path) -> Result<bool> {
    // Memoized (panel C9): repeated `cached_in` calls in one process (e.g.
    // config load + stage construction, or fetch fast-path + embedder load)
    // must not re-hash multi-hundred-MiB files. The memo key is the file
    // signature (per-file size + mtime, plus the `.complete` mtime), so any
    // external mutation or reinstall invalidates it — correctness is
    // preserved, only stat calls replace hashing on the hot repeat path.
    let stamp = verify_stamp(entry, dir)?;
    let memo = VERIFY_MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let map = memo
            .lock()
            .map_err(|poison| Error::internal(format!("verify memo lock poisoned: {poison}")))?;
        if let Some((verdict, cached_stamp)) = map.get(dir) {
            if *cached_stamp == stamp {
                return Ok(*verdict);
            }
        }
    }
    let verdict = verify_hashes_full(entry, dir)?;
    let mut map = memo
        .lock()
        .map_err(|poison| Error::internal(format!("verify memo lock poisoned: {poison}")))?;
    map.insert(dir.to_path_buf(), (verdict, stamp));
    Ok(verdict)
}

/// File signature used by the verification memo: presence + size + mtime of
/// every registry file and of the `.complete` marker.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifyStamp {
    complete: Option<std::time::SystemTime>,
    files: Vec<(String, Option<(u64, std::time::SystemTime)>)>,
}

fn verify_stamp(entry: &ModelEntry<'_>, dir: &Path) -> Result<VerifyStamp> {
    let mut files = Vec::with_capacity(entry.files.len());
    for spec in entry.files {
        let stamp = match std::fs::metadata(dir.join(spec.path)) {
            Ok(metadata) => {
                let modified = metadata.modified().map_err(|err| {
                    Error::internal_with_source(
                        format!("mtime {}", dir.join(spec.path).display()),
                        err,
                    )
                })?;
                Some((metadata.len(), modified))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(Error::internal_with_source(
                    format!("stat {}", dir.join(spec.path).display()),
                    err,
                ));
            }
        };
        files.push((spec.path.to_owned(), stamp));
    }
    let complete = match std::fs::metadata(dir.join(".complete")) {
        Ok(metadata) => {
            let modified = metadata.modified().map_err(|err| {
                Error::internal_with_source(
                    format!("mtime {}", dir.join(".complete").display()),
                    err,
                )
            })?;
            Some(modified)
        }
        Err(_) => None,
    };
    Ok(VerifyStamp { complete, files })
}

static VERIFY_MEMO: OnceLock<Mutex<HashMap<PathBuf, (bool, VerifyStamp)>>> = OnceLock::new();

/// Number of full sha256 verification passes (test-only counter for the C9
/// memo regression).
#[cfg(test)]
static HASH_PASSES: AtomicUsize = AtomicUsize::new(0);

/// The actual sha256 pass (see `verify_hashes` for the memo/wrapper).
fn verify_hashes_full(entry: &ModelEntry<'_>, dir: &Path) -> Result<bool> {
    #[cfg(test)]
    {
        HASH_PASSES.fetch_add(1, Ordering::Relaxed);
    }
    for spec in entry.files {
        let path = dir.join(spec.path);
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(Error::internal_with_source(
                    format!("stat {}", path.display()),
                    err,
                ));
            }
        };
        if metadata.len() != spec.size {
            return Ok(false);
        }
        if sha256_file(&path)? != spec.sha256 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Computes the hex sha256 of a file.
fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .map_err(|err| Error::internal_with_source(format!("open {}", path.display()), err))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| Error::internal_with_source(format!("read {}", path.display()), err))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Formats bytes as lower-case hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // `write!` on a String is infallible.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Expected download bytes for a model (sum of `files[].size`, FR-13).
#[must_use]
pub fn expected_bytes(entry: &ModelEntry<'_>) -> u64 {
    entry.total_size()
}

/// Pure preflight arithmetic: `available` bytes free versus the required
/// `1.5x expected + 256 MiB` (FR-13). Exposed separately so the error
/// wording and the threshold are unit-tested without a filesystem.
fn preflight_check(expected: u64, available: u64) -> Result<()> {
    // 1.5x expected, in integer arithmetic (floor equals the float truncation
    // for non-negative values).
    let scaled =
        expected.saturating_mul(PREFLIGHT_MULTIPLIER_NUMERATOR) / PREFLIGHT_MULTIPLIER_DENOMINATOR;
    let required = scaled.saturating_add(PREFLIGHT_HEADROOM_BYTES);
    if available < required {
        return Err(Error::Admission {
            message: format!(
                "needs ~{required} bytes free ({expected} for the files plus \
                 {PREFLIGHT_HEADROOM_BYTES} headroom); {available} are available — \
                 free space or point DOCSBASE_CACHE_DIR elsewhere"
            ),
        });
    }
    Ok(())
}

/// Free-space preflight for a directory (FR-13): `available` comes from the
/// platform seam.
fn preflight_free_space(entry: &ModelEntry<'_>, dir: &Path) -> Result<()> {
    let available = platform::fs::available_space(dir).map_err(|err| {
        Error::internal_with_source(format!("check free space on {}", dir.display()), err)
    })?;
    preflight_check(expected_bytes(entry), available)
}

/// Installs one file: stream the download to `*.part` while counting bytes
/// and hashing (round-3 mimo-4/muse-1: no whole-file RAM buffer), verify
/// size + sha256, fsync, then atomically rename into place. On any
/// mismatch the partial file is removed and an [`Error::Admission`] is
/// returned.
fn fetch_one(
    entry: &ModelEntry<'_>,
    source: &dyn ModelSource,
    dir: &Path,
    spec: &FileSpec,
) -> Result<()> {
    let target = dir.join(spec.path);
    let parent = target
        .parent()
        .ok_or_else(|| Error::internal(format!("model file path has no parent: {}", spec.path)))?;
    std::fs::create_dir_all(parent)
        .map_err(|err| Error::internal_with_source(format!("create {}", parent.display()), err))?;

    // Suffix via OsString push (lossy `display()` would mangle non-UTF-8
    // cache roots; round-3 mimo-3).
    let mut os_part = target.as_os_str().to_os_string();
    os_part.push(".part");
    let part = PathBuf::from(os_part);
    let mut file = std::fs::File::create(&part)
        .map_err(|err| Error::internal_with_source(format!("create {}", part.display()), err))?;
    let mut hasher = sha2::Sha256::new();
    let mut byte_len: u64 = 0;
    {
        let mut sink = HashCountSink {
            file: &mut file,
            hasher: &mut hasher,
            count: &mut byte_len,
        };
        source.fetch_to(entry.repo, entry.revision, spec.path, &mut sink)?;
    }
    if byte_len != spec.size {
        let _ = std::fs::remove_file(&part);
        return Err(Error::Admission {
            message: format!(
                "{}: size mismatch (registry {}, downloaded {}); re-run `docsbase models fetch {}`",
                spec.path, spec.size, byte_len, entry.name
            ),
        });
    }
    let digest = hex(&hasher.finalize());
    if digest != spec.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(Error::Admission {
            message: format!(
                "{}: sha256 mismatch (download corrupted); re-run with `--force`",
                spec.path
            ),
        });
    }
    file.sync_all()
        .map_err(|err| Error::internal_with_source(format!("fsync {}", part.display()), err))?;
    drop(file);
    std::fs::rename(&part, &target)
        .map_err(|err| Error::internal_with_source(format!("publish {}", target.display()), err))?;
    Ok(())
}

/// Write sink that forwards to a file while feeding a running sha256 and
/// byte counter (streaming fetch verification).
struct HashCountSink<'a> {
    file: &'a mut std::fs::File,
    hasher: &'a mut sha2::Sha256,
    count: &'a mut u64,
}

impl std::io::Write for HashCountSink<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf).inspect(|&written| {
            self.hasher.update(&buf[..written]);
            *self.count += written as u64;
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Writes the `.complete` marker atomically (temp + rename). It is written
/// strictly after every registry file exists (FR-6).
fn write_complete(dir: &Path) -> Result<()> {
    let complete = dir.join(".complete");
    // OsString suffix (round-3 mimo-3): lossy display() would mangle
    // non-UTF-8 cache roots.
    let mut os_tmp = complete.as_os_str().to_os_string();
    os_tmp.push(".tmp");
    let tmp = PathBuf::from(os_tmp);
    let file = std::fs::File::create(&tmp)
        .map_err(|err| Error::internal_with_source(format!("create {}", tmp.display()), err))?;
    file.sync_all()
        .map_err(|err| Error::internal_with_source(format!("fsync {}", tmp.display()), err))?;
    drop(file);
    std::fs::rename(&tmp, &complete).map_err(|err| {
        Error::internal_with_source(format!("publish {}", complete.display()), err)
    })?;
    Ok(())
}

/// The full install path: caps, preflight, per-file fetch, `.complete` last.
/// On any failure the revision directory is removed so nothing usable remains
/// (FR-6, P2).
fn install_all(entry: &ModelEntry<'_>, source: &dyn ModelSource, dir: &Path) -> Result<()> {
    if let Some(spec) = entry.files.iter().find(|spec| spec.size > MAX_FILE_BYTES) {
        return Err(Error::Admission {
            message: format!(
                "{}: file {} ({}) exceeds the per-file cap of {MAX_FILE_BYTES} bytes",
                entry.name, spec.path, spec.size
            ),
        });
    }
    if entry.total_size() > MAX_TOTAL_BYTES {
        return Err(Error::Admission {
            message: format!(
                "{}: total {} bytes exceeds the model cap of {MAX_TOTAL_BYTES} bytes",
                entry.name,
                entry.total_size()
            ),
        });
    }
    preflight_free_space(entry, dir)?;
    platform::fs::secure_dir(dir).map_err(|err| {
        Error::internal_with_source(format!("create cache dir {}", dir.display()), err)
    })?;
    for spec in entry.files {
        fetch_one(entry, source, dir, spec)?;
    }
    write_complete(dir)
}

/// Fetches `entry` into `dir`: a verified complete revision verifies under
/// the **shared** lock and returns without downloading; anything else takes
/// the exclusive install lock (FR-6). `force` always reinstalls.
///
/// The install itself runs in a sibling staging directory and is promoted by
/// rename only after every file verified. A failed fetch (including a failed
/// `--force` refetch) therefore never destroys a pre-existing verified
/// cache — the reviewer finding C6 fix.
pub(crate) fn fetch_into(
    entry: &ModelEntry<'_>,
    source: &dyn ModelSource,
    force: bool,
    dir: &Path,
) -> Result<()> {
    let complete = dir.join(".complete");
    if !force && complete.exists() {
        let file = open_lock(dir)?;
        let lock = RwLock::new(file);
        let _guard = lock.read();
        if verify_hashes(entry, dir)? {
            return Ok(());
        }
    }
    // The exclusive install path: make sure the model root exists before
    // the lock (the lock file lives at the root, stable across promotes).
    // Only secure a root we create ourselves; an existing root (e.g. a
    // test TempDir) must not be chmodded.
    let root = dir
        .parent()
        .ok_or_else(|| Error::internal(format!("cache path has no parent: {}", dir.display())))?;
    if !root.exists() {
        platform::fs::secure_dir(root).map_err(|err| {
            Error::internal_with_source(format!("create model root {}", root.display()), err)
        })?;
    }
    // Bounded try-retry (see `EXCLUSIVE_LOCK_RETRIES`): never block
    // silently on a process-lifetime shared holder (round-2 P1b).
    let Some(_guard) = acquire_exclusive_lock(dir)? else {
        return Err(Error::Admission {
            message: format!(
                "model \"{}\" is loaded/in use; stop the daemon (or retry) then re-run `docsbase models fetch {}`",
                entry.name, entry.name
            ),
        });
    };

    if !force && complete.exists() && verify_hashes(entry, dir)? {
        return Ok(());
    }

    // Staging/backup live as SIBLINGS of the model root (under
    // `<cache>/models`), not inside it (round-3 deepseek-4): a concurrent
    // `models remove` deletes the root subtree; a staging dir inside it
    // would be deleted mid-download and the root lock inode unlinked while
    // we hold it. Sibling staging is immune, and promote into a vanished
    // root fails cleanly (never a false "fetched" report).
    let parent = dir.parent().and_then(Path::parent).ok_or_else(|| {
        Error::internal(format!("cache path has no grandparent: {}", dir.display()))
    })?;
    platform::fs::secure_dir(parent).map_err(|err| {
        Error::internal_with_source(format!("create models dir {}", parent.display()), err)
    })?;
    // Unique staging/backup names per fetch: parallel fetches (or parallel
    // tests) must never share a `.staging-*` sibling.
    let tag = format!(
        "{}-{}",
        entry.revision_short(),
        STAGING_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let staging = parent.join(format!(".staging-{}-{tag}", entry.name));
    // Remove any stale staging dir from a crashed earlier attempt.
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    // Create the staging dir before the preflight (free-space checks need a
    // real path to stat); `install_all` re-secures it harmlessly.
    platform::fs::secure_dir(&staging).map_err(|err| {
        Error::internal_with_source(format!("create staging dir {}", staging.display()), err)
    })?;

    let result = install_all(entry, source, &staging);
    match result {
        Ok(()) => {
            // Promote the staging dir; move the previous dir aside first.
            // Note: a crash after this first rename leaves `dir` absent until
            // the publish rename completes — the best-effort restore below
            // covers the publish-rename failure path, not a hard kill.
            let backup = parent.join(format!(".old-{}-{tag}", entry.name));
            if dir.exists() {
                if backup.exists() {
                    let _ = std::fs::remove_dir_all(&backup);
                }
                std::fs::rename(dir, &backup).map_err(|err| {
                    Error::internal_with_source(format!("stash old cache {}", dir.display()), err)
                })?;
            }
            if let Err(err) = std::fs::rename(&staging, dir) {
                // Try to restore the old cache before surfacing the error.
                if dir.exists() {
                    let _ = std::fs::remove_dir_all(dir);
                }
                if backup.exists() {
                    let _ = std::fs::rename(&backup, dir);
                }
                return Err(Error::internal_with_source(
                    format!("publish cache {}", dir.display()),
                    err,
                ));
            }
            // Best-effort drop of the superseded cache.
            let _ = std::fs::remove_dir_all(&backup);
            Ok(())
        }
        Err(err) => {
            // Keep the previous cache untouched; drop only the staging dir.
            let _ = std::fs::remove_dir_all(&staging);
            Err(err)
        }
    }
}

/// Fetches a model into the cache (public entry point used by the CLI).
///
/// # Errors
/// Returns [`Error::Admission`] for unknown entries, transport failures,
/// hash/size mismatches, cap violations and preflight failures; the cache is
/// left without a usable partial install.
pub fn fetch(entry: &ModelEntry<'_>, source: &dyn ModelSource, force: bool) -> Result<PathBuf> {
    let dir = cache_dir(entry.name, entry.revision);
    fetch_into(entry, source, force, &dir)?;
    Ok(dir)
}

/// Whether a revision is cached and fully verified. `false` covers both a
/// missing `.complete` and any tampered file. This re-reads every file; the
/// cost is bounded by the registry sizes (NFR-3).
///
/// # Errors
/// Returns an internal error when file IO fails while verifying.
pub fn cached(entry: &ModelEntry<'_>) -> Result<bool> {
    let dir = cache_dir(entry.name, entry.revision);
    cached_in(entry, &dir)
}

/// Shallow presence check for reporting commands (`models list`, rounds-3
/// deepseek-7): `.complete` marker + per-file sizes only — no sha256 passes
/// (a one-shot CLI would otherwise re-hash multi-GiB models on every
/// invocation). The full hash stays on the trust/load path (NFR-3).
///
/// # Errors
/// [`Error::Internal`] when a file cannot be stat'ed for a non-NotFound
/// reason.
pub fn cached_shallow(entry: &ModelEntry<'_>) -> Result<bool> {
    let dir = cache_dir(entry.name, entry.revision);
    if !dir.join(".complete").exists() {
        return Ok(false);
    }
    for spec in entry.files {
        let path = dir.join(spec.path);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() == spec.size => {}
            Ok(_) => return Ok(false),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(Error::internal_with_source(
                    format!("stat {}", path.display()),
                    err,
                ));
            }
        }
    }
    Ok(true)
}

/// Same as [`cached`] against an explicit directory (test seam).
pub(crate) fn cached_in(entry: &ModelEntry<'_>, dir: &Path) -> Result<bool> {
    if !dir.join(".complete").exists() {
        return Ok(false);
    }
    verify_hashes(entry, dir)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    /// Deterministic fake source: per path it returns `path` repeated to the
    /// declared size (truncated at the end), so size and sha256 are
    /// reproducible.
    struct FakeSource {
        specs: Vec<FileSpec>,
        overrides: HashMap<String, Vec<u8>>,
        calls: Arc<AtomicUsize>,
        fail_all: bool,
    }

    impl FakeSource {
        fn new(entry: &ModelEntry<'_>) -> Self {
            Self {
                specs: entry.files.to_vec(),
                overrides: HashMap::new(),
                calls: Arc::new(AtomicUsize::new(0)),
                fail_all: false,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    /// Deterministic content: `path` bytes repeated to the declared size
    /// (truncated). Computed lazily so oversized cap-test entries never
    /// allocate their declared bytes.
    fn fake_content(spec: &FileSpec) -> Vec<u8> {
        let size = usize::try_from(spec.size).unwrap_or(usize::MAX);
        let block = spec.path.as_bytes();
        let mut content = Vec::with_capacity(size);
        while content.len() < size {
            let rest = size - content.len();
            content.extend_from_slice(&block[..block.len().min(rest)]);
        }
        content
    }

    impl ModelSource for FakeSource {
        fn fetch(&self, _repo: &str, _revision: &str, path: &str) -> Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_all {
                return Err(Error::Admission {
                    message: "fake transport failure".to_owned(),
                });
            }
            if let Some(data) = self.overrides.get(path) {
                return Ok(data.clone());
            }
            self.specs
                .iter()
                .find(|spec| spec.path == path)
                .map(fake_content)
                .ok_or_else(|| Error::Admission {
                    message: format!("fake source has no file {path}"),
                })
        }
    }

    /// A tiny synthetic entry (two files) with sizes small enough for tests.
    /// Builds `<tmp>/models/<name>/<rev>` — a realistic cache location whose
    /// parent is the model root (locks resolve to the model root, so the lock
    /// file never lands on a system directory like `/tmp`).
    fn fake_cache_dir(tmp: &Path) -> PathBuf {
        let entry = fake_entry();
        tmp.join("models").join(entry.name).join(entry.revision)
    }

    fn fake_entry() -> ModelEntry<'static> {
        // Content is fixed by `fake_content` (path bytes repeated to `size`);
        // these sha256 values were computed for the exact sizes below.
        let files: &[FileSpec] = &[
            FileSpec {
                path: "onnx/model.onnx",
                size: 4096,
                sha256: "db743c223182c9ef5ecefd77fbd39a0bb571068bae9d254c728d09d226db5902",
            },
            FileSpec {
                path: "tokenizer.json",
                size: 512,
                sha256: "81c41ed08f50596b10289d11454c080a74710904e632a2697449d3a7e7a8f2de",
            },
        ];
        ModelEntry {
            name: "fake-model",
            task: Task::Embedding,
            repo: "fake/repo",
            revision: "0123456789abcdef0123456789abcdef01234567",
            dim: Some(16),
            pooling: Some(Pooling::Mean),
            quantization: None,
            output_key: None,
            license: "MIT",
            query_prefix: "query: ",
            passage_prefix: "passage: ",
            prompt_version: 2,
            files,
        }
    }

    #[test]
    fn registry_well_formed() {
        let mut names = HashSet::new();
        for entry in REGISTRY {
            assert!(
                names.insert(entry.name),
                "duplicate registry name {}",
                entry.name
            );
            match entry.task {
                Task::Embedding => {
                    let dim = entry.dim.expect("embedding row has dims");
                    assert!(dim > 0, "dim must be positive");
                    assert!(
                        matches!(entry.pooling, Some(Pooling::Cls | Pooling::Mean)),
                        "embedding row has Cls/Mean pooling"
                    );
                }
                Task::Rerank => {
                    assert!(entry.dim.is_none(), "rerank row has no dims");
                    assert!(entry.pooling.is_none(), "rerank row has no pooling");
                }
            }
            assert!(!entry.files.is_empty(), "non-empty files[]");
            // At least the four fastembed tokenizer files (FR-5).
            for required in [
                "tokenizer.json",
                "config.json",
                "special_tokens_map.json",
                "tokenizer_config.json",
            ] {
                assert!(
                    entry.files.iter().any(|spec| spec.path == required),
                    "{} missing {required}",
                    entry.name
                );
            }
            // The ONNX file is present (authoritative set includes it).
            assert!(
                entry
                    .files
                    .iter()
                    .any(|spec| spec.path.starts_with("onnx/")),
                "{} missing an onnx file",
                entry.name
            );
            for spec in entry.files {
                assert!(spec.size > 0, "file size must be positive");
                assert_eq!(spec.sha256.len(), 64, "sha256 must be hex (64 chars)");
                assert!(
                    spec.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                    "sha256 must be hex"
                );
                assert!(spec.size <= MAX_FILE_BYTES);
            }
            assert!(entry.total_size() <= MAX_TOTAL_BYTES);
            assert!(!entry.repo.is_empty());
            assert!(entry.revision.len() >= 8, "pinned revision short");
            assert!(!entry.license.is_empty());
            assert!(entry.prompt_version >= 1);
        }
    }

    #[test]
    fn registry_prefixes_are_role_aware() {
        let e5 = REGISTRY
            .iter()
            .find(|entry| entry.name == "multilingual-e5-small")
            .expect("e5 row");
        assert_eq!(e5.query_prefix, "query: ");
        assert_eq!(e5.passage_prefix, "passage: ");
        let bge = REGISTRY
            .iter()
            .find(|entry| entry.name == "bge-small-en-v1.5")
            .expect("bge row");
        assert!(
            bge.query_prefix
                .starts_with("Represent this sentence for searching")
        );
        assert_eq!(bge.passage_prefix, "");
        for name in ["jina-reranker-v1-turbo-en", "bge-reranker-base"] {
            let rerank = REGISTRY
                .iter()
                .find(|entry| entry.name == name)
                .expect("rerank row");
            assert_eq!(rerank.query_prefix, "");
            assert_eq!(rerank.passage_prefix, "");
        }
    }

    #[test]
    fn entry_unknown_is_admission() {
        let err = entry("no-such-model").expect_err("unknown");
        assert!(matches!(err, Error::Admission { .. }));
    }

    #[test]
    fn canonical_id_includes_revision_and_prompt() {
        let entry = fake_entry();
        let id = canonical_id(&entry, 384, true);
        assert_eq!(id, "fastembed/fake-model@d384@l2@r01234567@p2");
        let raw = canonical_id(&entry, 16, false);
        assert!(raw.ends_with("@d16@raw@r01234567@p2"), "{raw}");
    }

    #[test]
    fn expected_bytes_sums_files() {
        let entry = fake_entry();
        assert_eq!(expected_bytes(&entry), 4096 + 512);
    }

    #[test]
    fn preflight_reports_required_and_available() {
        let expected = expected_bytes(&fake_entry());
        let scaled = expected.saturating_mul(PREFLIGHT_MULTIPLIER_NUMERATOR)
            / PREFLIGHT_MULTIPLIER_DENOMINATOR;
        let required = scaled.saturating_add(PREFLIGHT_HEADROOM_BYTES);
        let err = preflight_check(expected, 1000).expect_err("preflight");
        match &err {
            Error::Admission { message } => {
                assert!(
                    message.contains(&required.to_string()),
                    "names required bytes: {message}"
                );
                assert!(message.contains("1000"), "names available bytes: {message}");
            }
            other => panic!("expected admission, got {other:?}"),
        }
        assert!(
            preflight_check(expected, required).is_ok(),
            "exactly required passes"
        );
    }

    #[test]
    fn fetch_installs_all_files_and_completes_last() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");
        for spec in entry.files {
            let path = dir.join(spec.path);
            assert_eq!(
                std::fs::metadata(&path).expect("file").len(),
                spec.size,
                "{} installed with expected size",
                spec.path
            );
        }
        assert!(dir.join(".complete").exists(), ".complete written last");
        // `.complete` is written after the files: it must survive a failed
        // re-fetch only when the files verify.
        let source2 = FakeSource::new(&entry);
        fetch_into(&entry, &source2, false, &dir).expect("second fetch");
        assert_eq!(source2.calls(), 0, "second fetch downloads nothing");
        // No `.part` files remain.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "no partial files remain");
    }

    #[test]
    fn fetch_rejects_bad_hash_and_size() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let mut source = FakeSource::new(&entry);
        // Corrupt one file: return the wrong size.
        source
            .overrides
            .insert("tokenizer.json".to_owned(), vec![0_u8; 100]);
        let err = fetch_into(&entry, &source, false, &dir).expect_err("bad size");
        assert!(matches!(err, Error::Admission { .. }));
        assert!(
            !dir.join(".complete").exists(),
            "no .complete after failure"
        );
        // Staging installs leave at most the `.lock` control file (never
        // `.part`/`.staging-*`/files).
        assert_clean_failure_dir(&dir);

        // Hash mismatch: same size, different content.
        let tmp2 = TempDir::new().expect("tmp");
        let dir2 = fake_cache_dir(tmp2.path());
        let mut source2 = FakeSource::new(&entry);
        source2
            .overrides
            .insert("tokenizer.json".to_owned(), vec![b'x'; 512]);
        let err = fetch_into(&entry, &source2, false, &dir2).expect_err("bad hash");
        assert!(matches!(err, Error::Admission { .. }));
        assert!(!dir2.join(".complete").exists());
        assert_clean_failure_dir(&dir2);
    }

    #[test]
    fn fetch_rejects_file_over_cap() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let mut entry = fake_entry();
        entry.files = &[FileSpec {
            path: "onnx/huge.onnx",
            size: MAX_FILE_BYTES + 1,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        }];
        let source = FakeSource::new(&entry);
        let err = fetch_into(&entry, &source, false, &dir).expect_err("cap");
        assert!(matches!(err, Error::Admission { .. }));
        assert_clean_failure_dir(&dir);
    }

    #[test]
    fn fetch_rejects_total_over_cap() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        // Two files each under the per-file cap but summing past the 3 GiB
        // total cap (NFR-3). The fake source is lazy, so nothing allocates
        // the declared bytes.
        let mut entry = fake_entry();
        entry.files = &[
            FileSpec {
                path: "onnx/a.onnx",
                size: MAX_TOTAL_BYTES / 2 + 1,
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            },
            FileSpec {
                path: "onnx/b.onnx",
                size: MAX_TOTAL_BYTES / 2 + 1,
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            },
        ];
        let source = FakeSource::new(&entry);
        let err = fetch_into(&entry, &source, false, &dir).expect_err("total cap");
        assert!(matches!(err, Error::Admission { .. }));
        assert_eq!(source.calls(), 0, "caps fail before any download");
        assert_clean_failure_dir(&dir);
    }

    #[test]
    fn fetch_force_after_hash_failure() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");
        // Tamper with a file.
        let tampered = dir.join("tokenizer.json");
        std::fs::write(&tampered, b"tampered").expect("tamper");
        assert!(
            !verify_hashes(&entry, &dir).expect("verify"),
            "tampered file fails verification"
        );
        // `--force` reinstalls and succeeds.
        let force_source = FakeSource::new(&entry);
        fetch_into(&entry, &force_source, true, &dir).expect("force refetch");
        assert!(
            verify_hashes(&entry, &dir).expect("verify"),
            "files restored after force"
        );
        assert!(dir.join(".complete").exists());
    }

    #[test]
    fn cached_reverifies_hashes() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");
        assert!(cached_in(&entry, &dir).expect("cached"));
        // Missing marker → not cached.
        std::fs::remove_file(dir.join(".complete")).expect("remove marker");
        assert!(!cached_in(&entry, &dir).expect("not cached without marker"));
        // Restore marker, tamper with a registered file → not trusted.
        std::fs::write(dir.join(".complete"), b"").expect("marker");
        std::fs::write(dir.join("tokenizer.json"), b"tampered").expect("tamper");
        assert!(!cached_in(&entry, &dir).expect("tampered cache not trusted"));
    }

    #[test]
    fn cached_memoizes_verified_hashes_within_process() {
        // C9: repeated `cached_in` calls in one process must not re-hash;
        // tampering (even same-size) must still be detected via the
        // file-signature invalidation.
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");
        // Relative check around the second call only: the process-global
        // counter may be bumped by sibling hashing tests in parallel, so an
        // absolute delta is not stable; a memo hit must add zero passes
        // between two consecutive local calls.
        assert!(cached_in(&entry, &dir).expect("cached"));
        let after_first = HASH_PASSES.load(Ordering::Relaxed);
        assert!(cached_in(&entry, &dir).expect("cached memo"));
        assert_eq!(
            HASH_PASSES.load(Ordering::Relaxed),
            after_first,
            "memoized second check adds no hash pass"
        );
        // Same-size tamper: signature changes (mtime), so re-verification
        // runs and detects the mismatch.
        std::fs::write(dir.join("tokenizer.json"), vec![0_u8; 512]).expect("same-size tamper");
        assert!(!cached_in(&entry, &dir).expect("tampered not cached"));
    }

    #[test]
    fn fetch_complete_takes_shared_lock() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");

        // A loaded model holds the shared lock; a complete fetch must verify
        // and return under the shared lock (never require the exclusive one).
        let lock_file = open_lock(&dir).expect("lock");
        let lock = RwLock::new(lock_file);
        let _shared = lock.try_read().expect("shared lock acquired");

        let counting = FakeSource::new(&entry);
        let before = counting.calls();
        fetch_into(&entry, &counting, false, &dir).expect("complete fetch under shared lock");
        assert_eq!(counting.calls(), before, "no downloads on complete fetch");

        // While the shared guard is held, the exclusive lock (as `models
        // remove` would take it) must be refused.
        let mut exclusive = RwLock::new(open_lock(&dir).expect("lock"));
        assert!(
            exclusive.try_write().is_err(),
            "exclusive lock refused while a shared holder lives"
        );
    }

    #[test]
    fn fetch_force_refuses_boundedly_when_shared_lock_held() {
        // Round-2 P1b: the exclusive install lock must never block silently
        // on a loaded instance's process-lifetime shared guard — bounded
        // retries then Admission.
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");
        let lock_file = open_lock(&dir).expect("lock");
        let lock = RwLock::new(lock_file);
        let _shared = lock.try_read().expect("shared lock acquired");
        let started = std::time::Instant::now();
        let err = fetch_into(&entry, &source, true, &dir).expect_err("bounded refusal");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "refused promptly, not hung"
        );
        assert!(matches!(err, Error::Admission { .. }));
        assert!(err.to_string().contains("in use"), "{err}");
    }

    #[test]
    fn fetch_waits_for_exclusive_holder_then_returns() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");

        // Hold the exclusive lock (simulating `models remove`) and make sure
        // a fetch thread politely waits on the shared lock instead of failing
        // or forcing through.
        let mut exclusive = RwLock::new(open_lock(&dir).expect("lock"));
        let exclusive_guard = exclusive.try_write().expect("exclusive held");

        let dir_path = dir.clone();
        let entry = fake_entry();
        let counting = FakeSource::new(&entry);
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let result = fetch_into(&entry, &counting, false, &dir_path);
            let _ = tx.send(result);
        });
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "fetch must block while the exclusive lock is held"
        );
        drop(exclusive_guard);
        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fetch completes after release");
        result.expect("fetch ok");
        thread.join().expect("thread joins");
    }

    #[test]
    fn fetch_failure_leaves_no_partial() {
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let mut source = FakeSource::new(&entry);
        source.fail_all = true;
        let err = fetch_into(&entry, &source, false, &dir).expect_err("transport");
        assert!(matches!(err, Error::Admission { .. }));
        assert_clean_failure_dir(&dir);
    }

    /// Asserts a failed fetch leaves at most the `.lock` control file
    /// (staging installs leave no files/`.part`/`.staging-*`).
    fn assert_clean_failure_dir(dir: &Path) {
        if !dir.exists() {
            return;
        }
        for entry in std::fs::read_dir(dir).expect("dir") {
            let name = entry.expect("entry").file_name();
            let name = name.to_string_lossy();
            assert!(
                name == ".lock",
                "unexpected leftover {name} after failed fetch in {}",
                dir.display()
            );
        }
    }

    #[test]
    fn loaded_instance_blocked_remove_probe_across_refetch() {
        // Round-2 D2: the lock lives at the model root, so a successful
        // (unloaded) refetch does not change the lock file's identity; a
        // loaded instance's shared guard must keep blocking the remove-style
        // exclusive probe before AND after a refetch.
        let tmp = TempDir::new().expect("tmp");
        let dir = fake_cache_dir(tmp.path());
        let entry = fake_entry();
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");

        let shared = RwLock::new(open_lock(&dir).expect("lock"));
        let shared_guard = shared.try_read().expect("shared lock acquired");
        let mut exclusive = RwLock::new(open_lock(&dir).expect("lock"));
        assert!(
            exclusive.try_write().is_err(),
            "remove probe refused while loaded"
        );
        drop(shared_guard);

        // Unloaded refetch succeeds and the lock file is unchanged in
        // identity (same root path), so a fresh exclusive probe works.
        fetch_into(&entry, &source, true, &dir).expect("refetch");
        let mut exclusive2 = RwLock::new(open_lock(&dir).expect("lock"));
        assert!(
            exclusive2.try_write().is_ok(),
            "exclusive available after refetch"
        );
    }

    #[test]
    fn force_refetch_failure_preserves_verified_cache() {
        // C6: a failed `--force` refetch must not wipe the previously
        // verified cache (the old behavior removed the whole revision dir).
        let root = TempDir::new().expect("tmp");
        let entry = fake_entry();
        let dir = fake_cache_dir(root.path());
        let source = FakeSource::new(&entry);
        fetch_into(&entry, &source, false, &dir).expect("fetch");
        assert!(cached_in(&entry, &dir).expect("cached"));

        let mut failing = FakeSource::new(&entry);
        failing.fail_all = true;
        let err = fetch_into(&entry, &failing, true, &dir).expect_err("force refetch");
        assert!(matches!(err, Error::Admission { .. }));
        // The pre-existing verified cache is intact.
        assert!(cached_in(&entry, &dir).expect("cache preserved"));
        assert!(dir.join(".complete").exists());
        for spec in entry.files {
            assert_eq!(
                std::fs::metadata(dir.join(spec.path)).expect("file").len(),
                spec.size,
                "{} intact after failed force refetch",
                spec.path
            );
        }
        // No staging/backup leftovers: they live as SIBLINGS under
        // `<cache>/models` (round-3 deepseek-4), so scan that directory,
        // not the revision dir or the cache root.
        let models_dir = root.path().join("models");
        let leftovers: Vec<_> = std::fs::read_dir(&models_dir)
            .expect("read models dir")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !leftovers
                .iter()
                .any(|name| name.contains(".staging-") || name.contains(".old-")),
            "no sibling staging/backup dirs: {leftovers:?}"
        );
    }

    #[test]
    fn cache_dir_layout_includes_revision() {
        let dir = cache_dir("m", "0123456789abcdef");
        assert!(
            dir.ends_with(Path::new("models/m/0123456789abcdef")),
            "{dir:?}"
        );
    }
}
