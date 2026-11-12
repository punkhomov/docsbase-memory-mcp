# CF4c1: quiescent registry observation implementation plan

> **For agentic workers:** use superpowers:executing-plans inline. No implementation
> delegation; fresh task/final reviews are required by AGENTS.md.

**Goal:** получить полный bounded project registry snapshot без изменения metadata
DB/sidecars, чтобы последующий migration CLI не выдавал current cwd за inventory.
**Architecture:** metadata-only presence observation; затем private immutable
SQLite connection с native READWRITE descriptor без CREATE. До любого SQL взять
на ЭТОМ descriptor nonblocking SHARED→EXCLUSIVE lock, проверить отсутствие recovery
sidecars и стабильность namespace. Queries используют тот же descriptor; read-only
pager не снимает external lease. Connection close освобождает partial/full locks.
**Tech Stack:** Rust1.88, existing rusqlite/bundled SQLite3.53.2/serde/blake3/std.
**Spec:** component-configuration FR-9/10/12, NFR-1/3; cf4c-investigation.md.

## Global Constraints

- Rust 1.88, Linux/macOS/Windows, cli-full; lean default offline.
- No new crates, production unwrap/expect/panic or baseline/threshold changes.
- No DB creation/migration/recovery/checkpoint, sidecar/marker/conflict-log writes.
- No provider/model/daemon construction, no network/downloads.
- No process-global settings/context maps or dynamic VFS registrations.
- Caller supplies every inspection limit; active/recovery state stays unavailable.

## Task CF4c1 — actual bounded registry observer

**Files:** new `src/store/registry_inspect.rs`,
`src/store/registry_inspect/lease.rs`, `tests/registry_inspect.rs`; modify
`src/store/mod.rs`; add dated spec deltas/safety note and progress ledger.

**Interfaces:**

```rust
pub struct InspectionLimits {
    pub max_projects: usize,
    pub max_metadata_bytes: usize,
    pub max_sqlite_value_bytes: i32,
    pub sqlite_cache_kib: i32,
}
pub struct RegistryProject { pub id: i64, pub root: PathBuf }
pub struct RegistryObservation {
    pub observation_version: u32, // 1
    pub cache_root: PathBuf,
    pub state: RegistryState,
}
pub enum RegistryState {
    Missing,
    Unavailable(UnavailableReason),
    Ready { schema_version: u32, revision: String, projects: Vec<RegistryProject> },
}
pub enum UnavailableReason {
    Busy, RecoveryRequired, Changed, UnsupportedVfs, UnsupportedSchema,
    InvalidRegistry, LimitExceeded, Io,
}
pub fn inspect_registry(cache: &Path, settings: &Resolved<DbSettings>,
    limits: InspectionLimits) -> Result<RegistryObservation>;
```

Only invalid caller settings/limits/path admission returns Error. Observation
failures are typed Unavailable. Ready covers registry rows in the captured
snapshot only; no index/readiness/provider/runtime claims. Missing is distinct
from Ready(empty). Metadata-only preflight precedes SQL and never creates paths.

- [x] **RED**: `tests/registry_inspect.rs` uses the above API with actual Db/registry
  fixtures. Initially fail for missing module/API. Example acceptance:

```rust
let cache = tempfile::tempdir().unwrap();
let db = Db::open(cache.path()).unwrap();
let project = tempfile::tempdir().unwrap();
// Register through daemon::registry::ensure_project, then drop Db to checkpoint.
let before = snapshot_files(cache.path());
let report = inspect_registry(cache.path(), &settings(), limits()).unwrap();
assert!(matches!(report.state, RegistryState::Ready { .. }));
assert_eq!(snapshot_files(cache.path()), before);
```

- [x] Implement `InspectionLimits` admission: max_projects+1 representable as
  SQLite i64; positive metadata/value/cache bounds, value/cache within i32.
  Cache path must be absolute UTF-8, NUL-free and fit metadata budget. Logical
  retained metadata charge: UTF-8 cache path bytes + schema u32 (4) + each id i64
  (8) + UTF-8 project root bytes. Object allocator overhead/SQLite internal
  schema allocations are not claimed as strict RSS. Separate SQLite value cap
  permits exact output-boundary tests without constraining fixed schema DDL.
- [x] Validate passed Db settings and perform metadata-only presence/sidecar
  preflight; no raw DB descriptor opens. Missing maps to Missing, recovery and
  nonregular files stay unavailable. Schema is read only after native lease.
  Correction justified by POSIX rollback-reader regression: raw header inspector
  closes would release other SQLite connections' process locks. No defaults.
- [x] `lease.rs` owns the narrow FFI seam: open percent-encoded absolute filename
  URI with immutable=1 and flags READ_WRITE|URI|NO_MUTEX|PRIVATE_CACHE|NOFOLLOW,
  WITHOUT CREATE. immutable makes the pager read-only while the native descriptor
  can acquire write-style OS locks. No SQL before lease. Use FILE_POINTER and
  VFS_POINTER file controls, recognize only built-in unix/win32, call native xLock
  SHARED then EXCLUSIVE without an application retry loop (native Windows retries
  are an explicit pinned dependency exception; no wall-clock guarantee). Validate pointers/callbacks/statuses, keep
  connection private, no raw pointer escapes. Drop connection on every error;
  native xClose releases even partial locks. No separate reopen by filename.
- [x] Reject WAL/SHM/journal before SQL and after capture. Unix HAS_MOVED must
  report unchanged; win32's native no-FILE_SHARE_DELETE semantics must pass native
  replacement/locking tests. Unknown backend/control behavior is unavailable.
  Observation assumes cooperating SQLite writers; deliberate raw writes bypassing
  SQLite's lock protocol are outside the database coherence guarantee.
- [x] After lease, set SQLite LENGTH cap via existing FFI helper, cache_size to
  negative explicit cache KiB and mmap_size=0. Disabling mmap is a fixed inspection
  strategy, not a hidden budget fallback. Read user_version; mismatch is typed
  UnsupportedSchema without conflict log. Query only:

```sql
SELECT id, canonical_root FROM projects ORDER BY rowid LIMIT ?1
```

  Use max_projects+1 to detect incomplete inventory. Borrow row text before
  allocation, reject nonpositive/nonincreasing IDs, invalid UTF-8/NUL/nonabsolute
  roots, count/byte excess; then copy bounded roots. Missing project directories
  remain legitimate registered entries. Do not call operational list_projects,
  which loads statuses/coverage beyond this observation boundary.
- [x] Registry revision is versioned/domain-separated (`docsbase.registry.v1`):
  exact caller cache-path UTF-8 length+bytes, schema u32, then each ordered id i64
  and root length+bytes, fixed-width LE lengths. The cache path is an exact
  caller-selected context, so alternate spellings may conservatively differ.
  No content/settings/provider hash. Same rows in different cache roots differ.
- [x] Tests: missing directory untouched; closed persisted registry returns all
  roots (including unindexed/missing-on-disk); empty registry; active same/other
  process writer returns Unavailable; sidecars/hot journal never ignored; schema
  mismatch no writes/conflict log; count/byte exact boundaries and one less;
  raw SQLite over-limit value, malformed row; encoded filename `?/#/%/space/Unicode`;
  symlink no-follow; stable identity and changed membership/cache context.
- [x] Gated internal tests characterize pinned SQLite behavior: writer appears
  after connection open before lock; page size/auto_vacuum/rows change in that gap;
  competing write rejected THROUGH capture, accepted after observer drop; error
  path releases partial lock; Unix replacement between open and lock is refused;
  Windows replacement semantics tested natively. No sleeps for interleavings.
- [ ] GREEN targeted full/lean; full cli-full suite; full/lean Clippy/fmt/diff.
  Review safety argument and source evidence, fresh task review/fixes, commit,
  fresh final review, required native CI and artifact/inventory audit. User has
  authorized squash merge after verified reviewed head/tree; #78 stays open.

## Safety and publication limits

No SQL-derived rows exist before the exclusive native lease. Bundled BtreeOpen
reads only a bootstrap header, with pPage1 unset; lockBtree reconciles page size
on the first actual query. Pin this with the gated 4096→1024/autovacuum/insert
regression. No external writer is allowed while immutable rows are read. Connection
and native file are the SAME object, eliminating a second-open inode race. Before
publishing a result, namespace and sidecars are rechecked; later changes require
revision revalidation by future apply. This is not immutable over a live WAL DB.

No config migrate CLI, source writes, active-daemon observer, consent or executor
is added by CF4c1. Those remain the next slice after this observer is usable.

## Self-review

Scoped FR-10/12 and NFR-3 addressed by actual SQL rows/file snapshots and explicit
limits. Full registry from active WAL is deliberately unavailable. No manifest,
VFS registry or whole-DB copy. Global settings/defaults and artifact data versions
remain outside this task. All names/signatures above belong to this task; no new
crate or public runtime setting is required.

## Review corrections — 2026-11-12

- Check concrete Unix methods before xLock: pinned native posixIoMethods alone
  has xShmMap (iVersion>=2); refuse alternate tables, never call xShmMap. Actual
  unix-none/unix-dotfile regression, no global registration or mock methods.
- Resolve cache directory only, retain DB NOFOLLOW, and recheck caller directory
  resolution before Ready. Support macOS /var symlink ancestry; reject retarget.
- Persist a root larger than SQLite value cap that admits the normal schema;
  require LimitExceeded/no writes and demonstrate recovery with sufficient cap.

## Final review corrections

- Compare Windows borrowed native HANDLE normalized DOS filename to expected
  canonical-directory/DB entry before SQL and Ready; bounded output, no extra
  file open/close. Add narrow platform facade and gated symlink-after-preflight
  regression with target recovery journal. Native Windows verification required.
- Document pinned native Windows retry exception, never change global AV_RETRY.
- Canonicalize alternate-VFS test directory on macOS before direct NOFOLLOW open.
