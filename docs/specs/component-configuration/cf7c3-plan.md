# CF7c3 — prepared watcher runtime

> Execution: executing-plans/TDD in the existing isolated worktree; fresh task
> and independent final review. Source base d5601d4 (reviewed PR146), native
> delivery complete as c8d1e90 (PR146, after Node PR141). Integrated current
> origin/main before this PR; reviewed and integrated source trees identical.

Goal: watcher producer, retry and incremental/full recovery use one coherent
prepared Config/HybridStages/VectorCacheSource snapshot without rebuilding a
provider per batch. Rust1.100.0 stable; cli-full and lean offline. New crates:none.
Spec: component-configuration FR-2/3/5/16, SC-3; core R2/W103 and watcher OQ-6.
No production unwrap/expect/panic; no lock/baseline/threshold/tokenizer changes.

## Contract and alternatives

One immutable WatchRuntime contains config/stages/source. One mutex-protected
slot publishes a complete snapshot; producer reads only its config, consumer
captures the whole snapshot per writer-lease attempt. Separate mutable slots
would mix revisions; rebuilding stages per attempt would duplicate providers.
Old spawn_watcher/Watchers ensure/revise remain configured compatibility paths
through shared private cores. Legacy revise cannot downgrade a prepared watcher.

WatchRuntime::new(config,stages,source) does no provider construction. New
Watchers::ensure_with_runtime(cache,project,Arc<WatchRuntime>,db_settings) and
revise_with_runtime(cache,project_id,Arc<WatchRuntime>) admit root before resources
or replacement. A watcher belongs to one cache root; a foreign revision fails
without replacing it. Runtime stages are authoritative for embedding.

run_batch_using captures the snapshot under the writer lease, refilters raw
pending paths, and calls run_incremental_checked_with_embedding for prepared
mode. Compare complete snapshot identity at the existing pre-marker stale gate.
Recreated-index full fallback carries the same source/stages/gate. Retry recaptures
current context, with existing bounded backoff/coalescing. An already admitted
payload may finish with its old stages/source; reload need not wait for inference.
No slot/state/SQL transaction is held through providers.

Shared ensure prepares stages outside state/DB guards, then serializes watcher
creation with the per-project reload mutex and checks the captured config is
still current. A stale ensure retries and never replaces a newer watcher.
Reload prepares the runtime source before every successful publication (including
identical Config values) and revises the whole
watcher snapshot while retaining project publication authority. Initial startup
lists projects and releases the shared DB guard before preparing any providers.
Stage admission can now prevent watcher startup (existing warning path), rather
than deferring that failure to its first edit. Optional watcher resource failures
remain warnings. Production startup remains legacy; no strict/v2 activation.

## One reviewable task

Files: src/watch/mod.rs and new private src/watch/runtime_tests.rs;
src/daemon/{server,project_state_tests}.rs; dated specs and progress ledger.

- [x] RED public runtime API/admission and real prepared consumer owner test.
- [x] Shared producer/consumer/run_batch cores, compatibility paths retained.
- [x] Actual strict-owner incremental vectors and committed Tantivy outcome;
  queued old revision picks up new ignores/stages, no-op/provider reuse.
- [x] Stale prepared payload and recreated full recovery reject before markers/
  provider/generation changes; retry uses current snapshot; admitted old provider
  may finish during revision and can acquire registry/vector SQL write locks.
- [x] Provider error preserves retry hashes, then same-byte retry succeeds;
  source clones retain owner lifetime; invalid root preserves slot/resources.
- [x] Actual Shared ensure/reload use prepared source without rebuilding stages;
  stale ensure cannot overwrite later publication; existing watcher tests retained.
- [x] Focused and full cli-full/lean, full/lean all-target Clippy/fmt/diff,
  fresh task/final reviews.
- [ ] Required four native checks and parent/new inventory.
- [ ] PR reviewed/tested/delivered tree identity, authorized squash delivery;
  #78 stays open for strict startup/adoption/apply and remaining component owners.
