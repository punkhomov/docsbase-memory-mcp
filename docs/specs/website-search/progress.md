# #79 progress

2026-11-13: agent B starts client stage C from4c9ff9f in isolated worktree.
User authorized design/implementation/PR/merge continuously. No need to ask gates
again; independent machine reviews remain mandatory. #79 not complete after C.
Spec/design/tasks challenge: exactbuild required; read-only binding avoids index/
backfill; u64 losslessness; no replay; no arbitrary roots/tools; bounded queues;
sameuser/PID deployment; full corpus acceptance deferred explicitly, not waived.
A/B interface recorded in design.md. No storage/vector/index/settings migration
code changed. Server composition ownership to be settled per integration PR.

2026-11-13: plan review fix pass (до реализации, independent re-review pending).
Ruling: ingress timestamp/absolute deadline захватываются до validation и
serialization — preprocessing должен расходовать общий budget; C2 требует clock
tests на elapsed/remaining и отсутствие admission/I/O при expiry. Цена ошибки —
операция превышает заявленный timeout на время preprocessing.
Ruling: expectedSchemaVersion обязателен и сверяется с positive safe integer
Hello.schema_version до bind — build_id() содержит только Rust package version,
не source fingerprint; matching build не доказывает совпадение schema. C1 требует
config/Hello invalid/mismatch tests, включая matching build. Цена ошибки —
несовместимый daemon допускается до read-only binding/search.
Minor 3 исправлен в плане: chunk_id raw token ограничен unsigned integer syntax и
0..2^64-1, output canonical decimal string; C1 требует 0/MAX/MAX+1/negative/-0/
fraction/exponent tests. Production parser ещё не реализован.
Requirements/design/tasks изменены с dated delta markers. Production code/tests
не добавлялись; фиксируется план тестов, а не их выполнение. Fix diff и notes:
/workspace/scratch/agent-b/79/plan-fix.{diff,md}; independent review делает controller.

2026-11-13 C1 verification (review pending): actual RED c1-red.log shows missing
connection.ts; additional schema-range RED c1-schema-red.log. GREEN: Node24.22.3,
17 passed /0 failed; package tsc build/check exit0, diff check exit0. Evidence:
/workspace/scratch/agent-b/79/c1-{red,schema-red,green}.log. Complete mandatory policy,
read-only bind, exact schema/build/capabilities, capped contiguous framing buffer,
lossless u64 and runtime citation checks. Dev-only @types/node24.19.2 and existing
TS5.9.3; pnpm normal 14-day policy accepted. Generated dist/node_modules ignored.
C2/C3 and whole #79 remain incomplete.

Task C1: complete (2026-11-13): transport + checked DTOs/settings, actual RED→GREEN,
final27 passed/0 failed; tsc build/check, package-local Biome format check and
source diff check exit0. Web check0errors/0warnings; pnpm audit moderate gate0
(existing Web dev deepmerge low advisory unchanged). Fresh review initially found
3 Important: Windows UNC endpoint, unsafe score integer exemption, missing schema/
u64 boundary tests. Separate implementer fixed all with actual RED23pass/4fail
thenGREEN27/0; original reviewer scoped re-review PASS/APPROVED, no new findings.
Evidence /workspace/scratch/agent-b/79/c1/{review.md,fix-rereview.md,fix/}.
Ruling: native schema/version fields have the Rust u32 representability bound;
platform is a captured capability, not runtime configurable policy. Biome sibling
path invocation hit upstream ignore-root panic; package-local VCS-disabled scratch
config works without changing repo settings. No deferred C1 minors.

2026-11-13 C2 verification (review pending): RED missing index.ts, additional
RED pre-aborted work prepared args; RED custom tags.toJSON invoked twice.
Implemented ingress operation across preparation/queue/connect/Hello/bind/reply,
separate bounded FIFO pools, owned bounded search args, no replay/autostart/admin,
explicit close and typed errors. GREEN44/0 (17client+27transport), tsc build/check0,
diffcheck0. Deterministic clocks/timers cover preprocessing, Hello, queued expiry
and context elapsed/remaining/queue, without sleeps or latency assertions.
Evidence /workspace/scratch/agent-b/79/c2-{red,aborted-red,owned-red,green,build,check}.log.

Task C2: complete (2026-11-13): public typed client, search/control FIFO pools,
original operation and cancellation, no replay, explicit close, bounded owned args,
package API/docs. Fresh review Important: timer-only transport could write after
synchronous encoding/Hello decode crossed deadline. Separate implementer added
mandatory caller-owned OperationGuard and checks through connection/open/exchange;
4 actual late-frame regressions RED0/4→GREEN48/0, build/check/format/diff0. Original
reviewer scoped re-review ADDRESSED, PASS/APPROVED, no new findings. Evidence:
/workspace/scratch/agent-b/79/c2/{review.md,fix-rereview.md,fix/}. No deferred minors.
C3/native/package acceptance and FR-S* still pending; #79 remains open.

2026-11-13 user ruling: real corpus/load/machine пока предоставить не может,
«можно скипнуть наверно». FR-S4 measurement deferred, not verified and no longer
blocks shipping/queue. No capacity promises or synthetic replacement. Runtime
server FR-S1..S3 remains required before #79 implementation completion.

2026-11-13 C3 local resource ruling: source-exact Node50/0, native2/0 + lean
fixture1/0, packed JS/external TS consumer and full+opt-in Clippy passed. Full
Rust suite failed during linking, before executing tests: ENOSPC; agent B target
reached32GiB, filesystem100%. Evidence c3-full-suite.log/c3-verification.log.
Only own disposable target-agent-b removed (source/evidence/A target preserved).
Repeat local Rust verification with CARGO_PROFILE_DEV_DEBUG=0,
CARGO_PROFILE_TEST_DEBUG=0, CARGO_INCREMENTAL=0 to fit disk; debug assertions,
features, test expectations and CI profiles unchanged. No bypass/skip/rebaseline.

2026-11-13 C3 local verification (fresh review pending): Rust-generated fixture
RED0/1 + Node fixture RED0/2, canonical generated GREEN; full/lean preserve_order
ordering discrepancy reproduced then canonicalized without numeric coercion.
Native root alias/junction fixture exposed path-string assumption RED, corrected
with realpath; no SDK production change. Final Node50/0 + tsc build/check0, real
packed ESM/external TS consumer0, native full2/0 + lean fixture1/0, fmt/diff0,
full all-target and opt-in Clippy0. Local full cli-full (debug symbols off only)
1341 passed/0 failed/18 ignored across83targets; original374 source/input hashes
unchanged before/after. ~3GiB target versus32GiB failing debug-symbol target.
Evidence c3-full-counts.json, c3-source-sha256.json and c3-*.log in scratch79.
Native CI remains pending until PR exact head; Linux checks do not prove other OS.
Real20k run user-deferred; serverFR-S1..S3 remain next phases. #79 not closed.

2026-11-13 C3 Important review fix (scoped re-review pending): nightly's full
release cli-full suite now explicitly builds packages/client in its existing
preparation step. Audited all workflows and agent/local entrypoints; required
native CI and AGENTS already prepare the client. Corrected docs/WEB full-test
sequence and documented generated-export prerequisite in IPC/CI/current specs.
CLI-only builds, compile-only checks, lean and selected non-Node suites unchanged.
Actual isolated source-copy RED: public package import fails ERR_MODULE_NOT_FOUND
for absent dist/index.js (exit1). GREEN: actual pnpm package build exit0, same
unchanged import succeeds exit0. All-workflow actionlint, static prerequisite/
preserved-Cargo-command/check-name audit and source diff check exit0. Fix changes
only YAML/docs; no Cargo rerun or Node suite rerun needed after unchanged runtime
and tests. Evidence /workspace/scratch/agent-b/79/c3/fix/. Exact-head native CI
and original reviewer's scoped re-review remain pending; no new acceptance claim.

Task C3: local implementation complete (2026-11-13): source-native integration,
Rust-owned canonical fixture, packed JS/TS check, CI/docs. Fresh task review found
one Important: nightly full release tests lacked SDK build. Separate implementer
reproduced clean missingdist RED1→actualbuild/import GREEN0; fixed nightly + current
Web instructions, audited other selected-target workflows and actionlint0. Original
reviewer scoped re-review ADDRESSED/PASS/APPROVED, no new findings. Evidence
/workspace/scratch/agent-b/79/c3/{review.md,fix-rereview.md,fix/}. No deferred minors.
PR exact-head native4 and most-capable whole-stage review remain required before
merge; no other-OS or full #79 completion claim. Next rebase onto A's CF7 main254af73
and verify combined source; no changes to A-owned storage implementation here.

2026-11-13 nativeCI43422791379 head570d704: docs/linux/macos passed, Windows failed
new native fixture's project lookup before daemon startup/search. Original log
has no actual compared path spellings, so exact spelling cause not established.
Separate implementer replaced fixture-only string equality with followed bigint
filesystem dev/ino identity and exactly-one matching registered row; same-name
wrong root/ambiguity cannot select a project. Three test files only, no SDK/Rust
production changes. Actual Windows RED retained; extra lookup RED before fix;
Node55/0, local native2/0, build/check/format/diff0. Original C3 task scoped review
and most-capable final scoped review PASS/APPROVED, no new findings/minors.
Evidence /workspace/scratch/agent-b/79/windows-fix/. Fresh exact-head native4 still
required, no blind rerun or Windows skip. User-deferred corpus remains unverified.

2026-11-13 nativeCI43426206657 headf03027d: docs/linux/macos passed; Windows lookup
now passed and real daemon started, then fixture connected to an absent pipe.
Actual Windows ENOENT log retained, actual daemon pipe hash not printed. Before
any fix, standalone proof linked the production platform::pipe_name and showed
before/after cache creation via canonical alias changes its result; stale-parent
endpoint actualdaemon RED confirms dependency. Separate implementer changed only
three testfiles: matching-PID daemon.json.socket is authoritative before first
realclient; Windows bare pipe name qualified per serialization; restart asserts
stable recorded endpoint and existingclient reconnects. Initial absent/noCache
negative retained. Staleendpoint RED→GREEN, Node58/0, native2/0, build/check/format/
diff0. Original task and most-capable final scoped reviewers PASS/APPROVED, no new
findings. Evidence /workspace/scratch/agent-b/79/windows-endpoint-fix/.
No production SDK/Rust behavior change, no retry/skip; fresh exactheadnative4 pending.

## 2026-11-13 — client delivery and S-stage baseline

Task C delivery: complete. PR141 squash0c6d707, final source6b5f6a8 on main3b34732;
required run43434043274 docs/linux/macos/windows GREEN. Linux1383/0/18 +static2/0;
macOS1379/0/18, Windows1365/0/18; Node58/0 eachOS. Raw/JSON counts, ZIP digests,
tested tree, baseline inventory (+exact2native tests, no removals/new ignores),
407source hashes checked. Independent final acceptance CONVERGED for C; no open
findings/minors. Receipt /workspace/scratch/agent-b/79/final-acceptance-6b5f6a8.md.

Task S plan: draft; requirements FR-S1a..S3b/SC-S2 and server-plan.md. B owns this
daemon/lifecycle/watcher/IPC/CLI integration; A owns store/index/config migration.
User's autonomous authorization covers design/implementation/review/PR/merge;
FR-S4 real corpus stays explicitly deferred/unverified. S challenge review pending.

## 2026-11-13 — S plan architectural challenge amendment

Task S plan challenge: original review FAIL/CHANGES, no Critical, five Important
and three Minor findings; evidence
/workspace/scratch/agent-b/79/server-plan-review.md. Separate plan implementer
amended server-plan.md plus requirements/design/tasks only: same-state Notify
async cancellation; coalesced janitor polled with accept/stop; explicit owner
blocking capacity512 and checked lexical closed inventory; #143 -> #147 -> S1
CLI route/typed denial contract; full-profile overlay/optional grace0 semantics;
preserved daemon/admission.rs::Lease; named commands/targets and same-PR owner
inventory/matrix adoption audit. Shared hybrid background/provider helpers are
explicitly outside lexical physical-reserve claims until #81.

Readiness amendment records the specific A-owned all-writer outcome prerequisite:
existing ProjectLease, existing index_generation, strict actual counts and
attempt/scope/job attribution, with final status/outcome coherently published.
No stale Done/equal-generation/timestamp proof; later direct partial (including
no generation change) invalidates success. Full survey with skipped unchanged
may qualify; zero-error incremental/subset cannot promote Unknown/Partial.
Missing/legacy/malformed proof requires explicit Sync after producer lands;
latest failed/partial Sync remains not-ready after direct success until a later
successful full Sync, including no-op. Two metadata observations around a
read-only health check, changed tuple -> state_changed without retries/Repair.
S2 waits for that narrow A projection, not all CF11; S1 is independent and #81
can proceed if S2 is parked. No B storage/index runtime work is authorized by
this amendment.

Ruling: all eight original findings have concrete proposed plan fixes, pending
the original reviewer's scoped rereview. No implementation, runtime tests,
Cargo, commits or GitHub operations were performed by the plan implementer.
Evidence/diff/report: /workspace/scratch/agent-b/79/server-plan-fix/.
No runtime/native acceptance, capacity/latency or real ~20k claim; S1a/b/c still
require separate fresh reviews and one fully wired integration PR.

## 2026-11-13 — S1a #152 bounded-capture plan addendum

Source-confirmed #152 adds a narrow FR-S1b/S1a slice before runtime code:
actual trusted cap+1 input consumption, shared bounded primitive from the config
observer, same captured startup bytes for parse/stamp, bounded restart hashing
and distinct Missing/Loaded/Unavailable diagnostics. Current MAX_STATE_BYTES1MiB
remains the bootstrap boundary; configurable I52 is deferred. S1 must record
its actual-enforcement slice and runtime/diagnostic-only dependency effect.
Diagnostic stamps do not become CF5 authority/publication; no CF5/v2/source-write
or strict-activation scope is added. Deterministic counter/growth/stream and
oversized-restart-notice cases are planned; no huge-allocation/RSS/timing gates.

Ruling: this docs-only addendum awaits original reviewer scoped PASS before S1a
runtime coding and closes #152 with the existing S1 integration PR. I1..I5 and
three Minor plan contracts remain closed/untouched. Review package:
/workspace/scratch/agent-b/79/s1a/plan152/. No product/test code, Cargo, commits,
GitHub operations, runtime tests or build commands by the addendum implementer.

## 2026-11-13 — S1a implementation checkpoint (review pending)

Task S1a: implementation in progress on rebased main `e911fd2`; separate fresh
task review and final full gates pending. Added daemon owner/config composition,
global-only source/provenance/views/migration preservation, complete existing
resource consumer forwarding and strict captured-launch constructors. The shared
bounded byte primitive serves observer/legacy reads/launch/restart diagnostics;
observer adapter policies and existing SearchOperation/generation/singleflight
are unchanged. No S1b/c implementation or full CF12/#79 completion is claimed.

Actual RED: `daemon_settings` missing owner/launch APIs; low-level `config::capture`
missing bounded API; real `lifecycle::owner_connection_limit_is_enforced_by_the_accept_loop`
accepted a second connection for configured1. Optional CLI grace API RED,
recognized Website strict+legacy startup semantic RED, and explicit captured
Tokio worker-sum missing-API RED were also observed. Evidence:
`/workspace/scratch/agent-b/79/s1a/{owner,capture,connection,grace-cli,website-interim,executor-sum}-red.log`.
Focused semantic GREEN: bounded reader5; config32/config_migration14/
config_observation13/config_resolution24/config_runtime14/config_view6;
latest daemon_settings15/lifecycle11. All-target cli-full Clippy passed before
the final narrow executor-count addition; fresh full/lint/lean evidence follows
before completion/review. Node/Web preparation is unchanged from the confirmed
session build; no dependency/feature/golden edits.

Ruling: S1a resolves/presents Website data but common strict+legacy startup
explicitly refuses it before cache/admission/listener/resources. S1b removes this
intermediate refusal only with all backend fixed-project/hybrid/watcher/lifecycle
gates. Recognized valid Website never enters malformed-global Shared fallback;
malformed legacy global warning/default/error retention remains intentional.

Ruling: reuse existing BootstrapContext Tokio env/CPU resolver/source, check
blocking+captured async-worker count before lease/runtime, and pass exactly that
captured count into Builder.worker_threads; no validate-then-reread race, new
literal/default/whitelist or broad bootstrap adoption. Deterministic overflow
uses explicit captured facts, no ambient env race or resource allocation.

Ruling: duration validation checks the actual platform Instant bound; u64::MAX
milliseconds is representable on Linux, so no arbitrary additional ceiling is
invented. Shared explicit grace0 and pending0 remain valid as specified.

Ruling: S1a preserves the accepted lexical owner sum but claims no physical
control reserve. S1c must audit Windows interprocess async flush/dirty-Drop linger
with the transport contract before validating the closed helper inventory. No
worker admission, janitor orchestration, transport rewrite or A-owned runtime
work is included here. Adopted/pending/deferred inventory and diagnostic-only
state dependencies are updated in this slice; repeat them after S1b/c before the
single S1 integration PR ships.

### S1a verification checkpoint — fresh review pending

Task S1a: verified implementation checkpoint, **not complete before fresh task
review**. Final Debug cli-full observer inventory/execute: exit0, 1448 inventory
entries, 88 target summaries, 1430 passed / 0 failed / 18 ignored; execute193.550s.
Raw evidence: `/workspace/scratch/agent-b/79/s1a/observations/s1a-cli-full/`.
Final owner target has16 passing cases, including additional semantic RED/GREEN
for imported Website policy hiding changed Shared grace. Full suite ran this
semantic implementation; the only subsequent source edit corrected a stale
lifecycle comment, with fresh lint and lean checks afterwards.

Commands executed through the task's offline Docker/private-tmp wrapper, Rust1.100.0:

- `cargo test --locked --features cli-full -- --list` -> exit0 / 1448 names.
- `cargo test --locked --features cli-full` -> exit0 / 1430 passed,18 ignored.
- `cargo test --locked --test daemon_settings --test config_resolution` -> exit0,
  owner16 + resolution23, no full lean suite.
- `cargo test --locked --lib config::capture` -> exit0 / 5 passed.
- `cargo clippy --locked --features cli-full --all-targets -- -D warnings` -> exit0.
- `cargo clippy --locked --features cli-full --test perf_budget --test artifact -- -D warnings` -> exit0.
- `cargo fmt --check` -> exit0. `git diff --check` and final source/inventory audit
  are included in the implementation review package.

Sourceguard baseline `5f86e46` -> current source: 1437 -> 1462 test names,
25 additions / 0 removals; 18 ignore attributes unchanged. Source scan/classification
is saved in `s1a/source-scan.txt` and the component inventory/state matrix. No new
crates/features, retrieval/golden changes, release/LTO run, model downloads or
paid API calls. Windows/macOS runtime acceptance and fresh review remain pending;
S1a cannot ship apart from connected S1b/c and their integrated final review.

### S1a fix round 1 — three Important review findings

Fresh Task review at `cd28b8b`: spec FAIL / quality CHANGES, three Important,
no Critical/Minor; `/workspace/scratch/agent-b/79/s1a/task-review.md`. This bounded
round addresses I1/I2/I3; original reviewer's scoped rereview remains pending.
No task-complete claim is made.

Actual semantic REDs in `s1a/fix1/`: `i1-i2-red.log` shows five invalid file-policy
CLI starts succeeded and created cache state, and omitted/explicit default Shared
grace produced different identities; `i1-shared-red.log` shows the same Website
file-policy bypass through legacy Shared. I3 first had a missing seam API RED,
then a faithful old-loop extraction produced `i3-semantic-red.log`: fake clock
advanced60s for timeout1ms/poll60000ms, with no wall-clock threshold/new Tokio
feature. Only then were behavior fixes applied. An initial GREEN compile attempt
had a stray generated comment character; it was corrected, not counted as RED.

I1: one canonical TOML document parse feeds typed daemon decoding/validation
before legacy fields. Error classification preserves new-owner failures and
recognized Website selection through later legacy-field errors; neither may enter
Shared defaults. Existing malformed Shared source warning/default/global-error
diagnostic behavior remains, including explicit Shared plus bad legacy fields.
The once-computed ConfigSpec defaults/provenance catalog is retained; no regex,
second source parse/read, private fallback/default or v2/apply behavior is added.

I2: a named effective-policy projection is shared by runtime hashing and impact
equality. Applicability marker/provenance and provided-snapshot validation remain
separate; omitted vs explicit Shared5000 has equal identity/no runtime action,
while origins differ and Website explicit0/5000 are still invalid. Byte-change
restart diagnostics remain a source observation, not a runtime identity.

I3: the existing reply-drain consumer clamps each sleep to the original deadline's
remaining budget. Injected clock/sleeper tests exercise the actual loop, not a
copied model. No physical Activity/worker drain, queue, transport or #155 work
is included; S1c scope remains deferred.

Focused GREEN: config32/migration14/observation13/resolution24/runtime14/view6,
daemon_settings18/lifecycle11; server unit slice17, including both new legacy
source cases and deterministic drain. Final full/lean/lint/inventory gates follow
before the fix checkpoint and scoped rereview package.

Fix round1 final verification (still pending original scoped rereview): full Debug
cli-full inventory1453 / execute1435 passed,0 failed,18 ignored,88 target summaries,
183.910s, both command exits0. Raw:
`s1a/fix1/observations/s1a-fix1-cli-full/{inventory,execute}.{json,log}`.
Lean owner18 + resolution23 passed; after interruption the completed original log
was preserved and a warm completion check explicitly returned0. Lean server17
(including deterministic drain/legacy policy tests) and capture5 passed. Final
cli-full all-target and explicit artifact/perf_budget Clippy -D warnings passed;
fmt/protected-path/diff/source audits are recorded in the fix package.

Sourceguard at taskbase `cd28b8b`:1462 -> 1467 source names,5 additions / 0 removals,
18 ignore attributes unchanged; no Rust source changes during or after final full
verification. Base remains `cd28b8b` despite newer main; no dependency integration
or rebase was mixed into this round. Original I1/I2/I3 approval remains the next
gate; neither S1a nor the later integrated S1/#79 is marked complete here.

### 2026-11-13 — S1a local task review complete; integration separate

Task S1a: complete (local implementation `5f86e46..290df36`; full Debug cli-full
1435 passed / 0 failed / 18 ignored, inventory1453; lean owner18/resolution23/
server17/capture5; current Clippy/fmt/diff/protected audits passed). Original fresh
Task review FAIL/CHANGES was fixed in one bounded round; original scoped rereview
at `/workspace/scratch/agent-b/79/s1a/fix1/rereview.md` is spec PASS / quality
APPROVED, I1/I2/I3 addressed, no new Critical/Important and no minors recorded.

Ruling: completion is limited to the reviewed local S1a owner/capture slice on
its tested base. Current-main plus local #147 source integration is a separate
checkpoint before S1b coding. Local #147 source is not a claim that PR157 is
merged or native-qualified; S1b/c, their reviews, public dependency/native gates
and the single S1 integration PR remain pending. No full CF12/#79 completion,
S2 readiness or Windows #155 physical-reserve/transport proof is claimed.


### 2026-11-13 — S1a integration gate passed; S1b implementation

S1a integrated checkpoint `17494b4` (tree `e357af1`) is independently reviewed:
spec PASS / quality APPROVED, no Critical/Important/Minor findings. The fresh
integration review is `/workspace/scratch/agent-b/79/s1a/integrated147/review.md`.
#147 landed as `92c3b6` with all four native checks; B-only reparenting preserved
the verified tree. S1b BASE is `17494b43a198cf4f856e998c218dceed9dfeb809`.

Task S1b: implementation in progress; no completion or public S1 ship claim.
Actual REDs cover the former startup refusal, missing no-diagnostics reader API,
immutable watcher owner API, pure idle transition, absent CLI selector,
unavailable Website direct sync, mutating live-Website CLI binding, async CLI
search binding and the live-Shared/file-Website fallback distinction. Raw logs
are in `/workspace/scratch/agent-b/79/s1b/`; current focused GREEN includes real
index edit/explicit sync/restart with watchers0 and ordinary Shared lifecycle.

Ruling: Hello's additive `active-website-profile-v1` reports ACTIVE Website only,
with Shared shape/trace and required SDK capabilities preserved. Reuse Admission
`-32010`; do not encode policy denial as Project/direct-fallback permission.
A narrow crate-internal readonly/no-diagnostics store entry reuses actual SQLite
schema/live-WAL validation; legacy readonly diagnostics remain. Refusal tests
check concrete absence of registry mutations, conflict/listener/runtime state or
provider/vector/ANN effects; valid startup still opens its ordinary resources.

Ruling: S1c physical worker lanes, pending admission, coalesced janitor and #155
transport work remain separate. No physical reserve, full CF12, readiness DTO,
v2/consent activation or complete #79 claim follows from S1b functional gates.


Ruling (S1b Web composition): source audit found Web's unconditional initial and
registry-wide `BindReadOnly(None)` conflicted with the fixed-ID backend gate.
Actual native-socket RED reproduced Admission at connection setup. Web now
validates panel-read capability before any panel calls, skips null binds only
for active-Website registry-wide reads, and retains real fixed-ID/project-required
binding admission. Shared framing/order, explicit wrong/None IPC denial, SDK API
and S2 scope remain unchanged. Socket regression and affected Web/CLI/backend
suites are included in this task's verification; no Windows transport change.

### 2026-11-13 — S1b verified checkpoint; fresh task review pending

Task S1b: verified implementation checkpoint, **not complete before fresh task
review PASS / quality APPROVED**. Frozen Debug cli-full observer: 1496 passed /
0 failed / 18 ignored, 91 target records, 253.365s including relinking; inventory
1514. Lean focused CLI18/profile6/owner18/resolution23/policy7 (72 total), backend
profile8 and admission reader3 passed. Full library532 and focused integration77
were also observed. Current all-target and opt-in artifact/perf Clippy, fmt and
diff checks passed; no release/LTO/performance run was required or performed.

Source/test freeze hashes for372 files are unchanged across final verification.
All non-S1b paths match BASE exactly; no crate/feature/fixture/golden/threshold or
ignore change. Raw inventory adds28 names and removes1: the former S1a temporary
startup-refusal test is renamed to the retained missing-registry/no-resources
admission test. Net27 tests; all18 ignored identities/reasons are unchanged. The
comments-aware source declaration scan finds1515→1542; raw Cargo inventory is the
authoritative executable count. Evidence/report/manifest are in
`/workspace/scratch/agent-b/79/s1b/`.

Ruling: scope remains S1b only. S1c physical admission/janitor/#155, integrated
final/native S1 review and the single public S1 PR remain pending. This local
checkpoint does not claim S2 readiness, full CF12/#79 completion or ship approval.

### 2026-11-13 — S1b fix round 1, I1 verified; scoped rereview pending

The original S1b task review found one Important I1: blocking native connection
could succeed, then timeout setup failure collapsed into absence and allowed CLI
direct indexing. `Client::try_connect` now preserves setup failures as Transport;
strict sync/status/list use it. The legacy Option probe retains its documented
lossy compatibility behavior. Async search/Web have no analogous client-layer
post-connect setup step and remain unchanged.

Actual deterministic semantic RED: injected setup failure after real native
connect returned absence and invoked the fixture-owned CLI direct action,
creating registry state (2 failed / 2 controls passed). GREEN: all4 new tests
pass, covering read/write setup errors, no direct action/state before Hello,
actual absence and successful setup ordering. Affected cli-full suites pass61
(CLI routing18/read14, profile6, IPC server18, error5); current all-target Clippy,
fmt and diff checks pass. No full1496 rerun: combined S1 verification follows
worker integration. Evidence is `/workspace/scratch/agent-b/79/s1b/fix1/`.

Task S1b remains pending original scoped rereview PASS/APPROVED. Deferred Minor
M1 (#155 wording) stays with the upcoming functional S1c commit; this fix changes
no Windows transport source, setting, feature, fixture, golden, threshold or
ignore identity and creates no separate documentation PR.

### 2026-11-13 — S1b fix round 2, fixture I2 verified; rereview pending

Original I1 rereview is spec PASS/addressed; quality found one new Important I2
in the test fixture's accepted-stream read/timeout setup on macOS. The fixture
now only holds a bound native listener. Setup callbacks prove native connection;
the four existing tests retain strict Transport/no-direct-action/no-state,
absence and callback-order assertions. Accepted-stream IO, threads, sleeps and
retries are removed. I1 production files remain byte-identical to `63df8aa`.
Targeted cli-full `cli::connection_tests`: 4 passed / 0 failed; fmt/diff checks
passed. No full repeat or native probe. Evidence:
`/workspace/scratch/agent-b/79/s1b/fix2/`. Original scoped rereview remains pending.

### 2026-11-13 — S1c in progress; corrected functional scope

Task S1c: in progress from671f2a7. Initial RED in server_workers failed only for
missing WorkerAdmission and same-token async cancellation APIs. Same-state Notify
unit GREEN:2 passed/0failed; generated Web/SDK assets were prepared before Cargo.
Raw evidence is in /workspace/scratch/agent-b/79/s1c/. Server/lifecycle wiring is
held until the S1b IPC finding is fixed and independently re-reviewed.

Ruling: this functional task corrects the earlier scope wording that grouped
platform adapter work into S1c. S1c bounds application-owned blocking closures
through actual finish/unwind, preserving original deadlines and cancellation.
The checked sum is not a process-wide/native IPC retirement or unconditional
control availability claim. Independent platform adapter work is parked outside
this task and is not a blocker. One server profile+workers PR closes #152; one
readiness+HTTP PR follows to close #79. No docs-only PR chain or #47/#48/#117/#128
expansion; #12 is closed not planned.


### 2026-11-13 — S1c application wiring checkpoint

Task S1c: functional implementation checkpoint, fresh review/final gates pending.
Independent admission target GREEN8/0; actual semantic RED4/4 showed prior bind,
register, metadata and search bypassing full lanes. After wiring all application
paths, focused daemon GREEN104/0 includes closure detach/unwind and coalesced
janitor accept/Stop/signal/physical-completion gates. Harness endpoint setup was
corrected with the existing state-directory helper; no native transport change.
The running-janitor seam had its own missing-API RED before implementation.

Ruling: common request::spawn moves the lane/sync guard and existing Activity
into actual execution; capacity releases before Activity reports completion.
Search admission uses the original deadline/token; queued caller drop cancels
that token. Website fixed sync acquires its singleton before enqueue and retains
the parent Activity through completion; legacy Shared background orchestration
remains outside the application reserve proof. Tick only installs one tracked
future; admission/join completion poll beside accepts and shutdown without the
former accept-first biased select. Existing daemon-lifetime Lease is untouched.


S1c integrated checkpoint now includes approved S1b production+fixture fixes
through74e8376, preserving both review ledgers. Current focused cli-full targets
server_workers/ipc_server/lifecycle/admission/admission_ux/server_profile/
daemon_settings pass. Current Clippy/fmt precede the clean functional checkpoint;
remaining gates are frozen full cli-full, lean focused/clippies, inventory/source
audit and fresh S1c review. This is not Task S1c completion or native ship approval.
Final receipts may live in the task report/PR without a documentation-only rerun.


### 2026-11-13 — PR158 final-review I1 focused fix

An explicit CLI Website selector now propagates a captured legacy global-source
error before admission/resources; warning/default fallback remains Shared-only.
Causal RED with an existing real indexed project observed endpoint/state
publication for malformed, oversized, unavailable and invalid Shared legacy
sources. Focused GREEN83/0 preserves Shared fallback, valid Shared overlay of
Website, recognized Website source-policy rejection and grace/provenance rules.
Current all-target Clippy, fmt and diff checks pass. Full current cli-full/lean
verification and original final scoped rereview remain pending. Evidence:
`/workspace/scratch/agent-b/79/server-delivery/fix1/`. No dependency, budget,
ignore, native adapter or S2 scope change.

### 2026-11-13 — S2 functional checkpoint; final verification/review pending

Task S2: implemented fixed-project read-only readiness/CLI/SDK after actual API,
CLI and causal association REDs. Targeted Rust: readiness units10 + integration41
passed; SDK63 passed; current cli-full all-target Clippy/fmt/diff checks passed.
The durable Sync association records A's exact attempt/actual counters before the
existing writer lease ends; bounded O1/health/O2 observation never repairs/retries.
Full cli-full, lean, inventory/source freeze and fresh task review remain pending.
Ready covers the indexed corpus/recipe, not watcher-off filesystem freshness or
continuous availability. S3 HTTP, native final delivery and deferred FR-S4 remain separate.

### 2026-11-13 — S2 accepted; S3/S4 functional checkpoint

Task S2: complete (bc845df; fresh review PASS/APPROVED, no findings; exact native
checks docs/linux/macos/windows green). Readiness scope and FR-S4 deferral stand.
Task S3/S4: functional HTTP/original-budget/native-preparation/docs bundle
implemented after SDK and HTTP missing-contract REDs. SDK69 and HTTP8 pass;
real native website_http1 + node_client2 pass; current cli-full all-target Clippy,
fmt/diff and actionlint pass. Full/lean inventories, final Node/artifact checks,
fresh task review and combined native/final delivery gates remain pending.
Ruling: known-closed idle SDK connections may be retired before dispatch; native
restart assertions now fail both pools while absent, then verify later fresh
calls. This corrected a test assumption; production no-replay behavior is intact.

### 2026-11-14 — S3/final review fix1 checkpoint

Task S3 fix1: addressed Important I1/I2 and companion M1 in one HTTP lifecycle
pass. Actual causal RED7/old controls8; GREEN HTTP15, SDK69 and native1+2, with
current check/build/format/fmt/diff passing. Shared finish/close/error/abort
settlement retains success/failure ownership; HTTP's original abort reason governs
504. Already-expired error writes get one immediate attempt, without flush grace.
Full cli-full/freeze proof and both original scoped reviews remain pending.

MODIFIED 2026-11-16 — #155 functional WIP checkpoint: architectural v3 reviewed;
actual Windows application T1 at da44095 failed the expected connection-charge
assertion with1,048,672 unread bytes/activity0, then drained the full1,048,673-byte
reply before cleanup. This is accounting evidence, not native terminal proof.
Daemon scoped/pre-frame replies and both IPC clients now use fixed preallocated
capped streaming serialization; error fallback reuses that region after dropping
the result. Focused encoding2/0. Owned IOCP/helper retirement, caller roots,
coordinator/Stop acknowledgement and native terminal acceptance remain WIP;
#155 is not complete and the diagnostic T1 remains intentionally RED until its
retarget to actual terminal ownership. No policy/baseline/ignore change.

MODIFIED 2026-11-17 — #155 native-core WIP: private raw IOCP core now owns
reserved physical-handle tokens, stable OVERLAPPED/buffer slots and packet
references installed before submission. Submission return and native completion
are separate gates. Cancellation uses handle→slot lock order and charges each
cancel caller until return, fencing buffer reuse and handle close. Three native-
compiled state tests cover completion-before-return, duplicate completion and a
delayed cancellation caller; they are not pending-I/O/physical-retirement proof.
Windows ABI/state execution is pending the isolated exact-source harness. Four
first-draft Clippy findings are corrected in-task. Stream/helper ownership,
outer-root migration and final normal native gates remain WIP; no clean-retirement
or #155 completion claim. Existing test/baseline/ignore budgets unchanged.

MODIFIED 2026-11-17 — #155 helper WIP: a dedicated connection-owned flush
worker now has fixed request/return events, explicit ticket/return phases and
retained pipe/thread ownership. Native writes are fenced until helper Ready;
helper/cancel-callers survive cancellation, and outer join observes actual
thread signaling. No dependency linger or pooled-thread cancellation is used by
this private helper. Existing windows-sys IO/Security features are explicit;
no new crate. Typed empty-byte comparison and the invalid Windows thread-trait
import found by native compilation are corrected; focused encoding2/0 again.
Native helper execution and full stream/root integration remain pending.

MODIFIED 2026-11-17 — #155 bounded core-review correction checkpoint: C1 now
uses the allocation's original mutable pointer before native submission, never a
shared-slice cast. I1 publishes permanent driver/helper fault gates to existing
handles and new admission, exposes owner fault, and retains fatal owners. I2
replaces callback-under-lock consumption with bounded read copying/non-read
consumption; Waker clone/drop also occurs outside those locks. Five controlled
core cases are queued for native validation; local encoding2/0 and unchanged
platform-boundary2/0 pass. The same named T1 fixture now queries native unread
bytes only through the platform seam. Raw owned connection code and a normal
read/write/helper-drain/token-transfer case are written but native execution and
outer-root integration remain pending. #165 deterministic grace-fixture work is
assigned to the same lifecycle acceptance; no grace/budget/ignore change.

MODIFIED 2026-11-17 — #155 scoped core fix2: dedicated-helper retirement now
publishes its joined success/error outcome persistently. Repeated outer drive
passes preserve that result without a second join or helper-charge release;
concurrent cancellation remains fenced through HANDLE clearing and publication.
A new actual raw-pipe case holds a native cancellation caller past helper join
and checks repeated retirement observations. Windows execution is pending;
current formatting passes. This is a correction of the core-review Important
finding, not a completed transport/root delivery claim.

MODIFIED 2026-11-17 — #155 physical-owner WIP: the bounded IOCP registry
now retains the listening/connection envelope before cancellable exposure and
before helper startup. A transfer preserves the physical token; replaced
envelopes and terminal owners drop outside the registry lock. A shared service
capability supplies admission, irreversible quiescence/abort and outer reaping;
escaped capabilities cannot reopen a finished scope. Logical stream Drop only
cancels/wakes; registered buffers and helper handles stay with the outer root.
A scoped runner drives native/helper retirement outside Tokio on work return or
unwind and retains quarantine after an observation interval. Three actual
connection cases plus one outer error/unwind case now await native execution;
Linux encoding2/0 and formatting pass. Operational daemon/client facade,
Activity/permit and complete policy migration remain WIP, not #155 acceptance.

MODIFIED 2026-11-17 — #155/PR #164 research checkpoint: compared Microsoft
cancellation/pipe lifecycle documentation, pinned libuv v1.x source and v2
shutdown removal, Mio #1944, compio/Asio ownership contracts, Windows threadpool
I/O and AF_UNIX alternatives. `windows-transport-research.md` records the three
acceptance/write-completion/peer-drain boundaries, a candidate separating normal
write completion from explicit normal-close drain, resource/performance
hypotheses and discriminating native tests. No application implementation or
native acceptance changed; no final design, finite-retirement or completion
claim. Existing wire format, test budgets and baseline remain unchanged.
Ruling: fresh research review PASS / APPROVED, no findings. Native lifetime,
deadline, helper-capacity, delivery, performance and #165 acceptance remain
unverified implementation work; this review does not qualify PR #164 for merge.

Task W1: complete (2026-11-17, proportional capped serialization). Actual Hello
allocation RED8,388,609 vs98 bytes; GREEN encoding4/0 and scoped7/0, lean library
Clippy with -D warnings and fmt pass. Both passes enforce their storage bound;
escaped overflow is rejected before output allocation. Oversized responses drop
before allocating the small fallback. No wire/baseline/threshold change.
Ruling: fresh task review PASS / APPROVED, no findings. Whole Windows transport
and final cli-full/native acceptance remain WIP; W1 is not #155 completion.

MODIFIED 2026-11-17 — W2 RED preparation: added an actual raw-pipe repeated
exchange asserting native write completion without an eager/drain helper.
Existing adapter still eagerly creates helpers; native RED execution is pending.
This intermediate commit is intentionally not feature acceptance.

MODIFIED 2026-11-17 — W2 implementation checkpoint: native Windows RED run
44097740879 compiled source61326a4 and failed the intended helper-absence
assertion (true/true vs false/false), after cleanup; external watchdog unused.
The private adapter now completes native writes without a helper and creates
one reserved dedicated helper only for explicit final server shutdown. Client
shutdown completes its writes only; closed writers reject further output.
Tests cover exact multi-chunk final delivery, optional-helper retirement and
actual NOT_FOUND cancellation in a controlled pre-entry interval. Exact-source
Windows cross-typecheck and fmt pass; native GREEN and task review are pending.
This is not operational integration or #155 acceptance.

MODIFIED 2026-11-17 — W2 review correction: candidate9c473fb passed all12
actual Windows core/root cases in run44099804924. Fresh static review found an
Important unchecked-writer-clone race and Minor noncausal final-read ordering.
Pipe is now nonclonable and each Connection permits only one logical stream;
Tokio split owns that stream. The final delivery case gates client reads at an
exact native-region boundary, retaining the final newline in the kernel until
a pending final-drain observation. Fix cross-typecheck/fmt pass; fixed-source
native execution and re-review are pending, so W2 is not yet marked complete.

MODIFIED 2026-11-17 — W3a RED preparation: added an actual owned-pipe
case requiring physical retirement while synchronous application work is still
running, without cooperative async yield. The existing run_on driver only reaps
between work polls, so this distinguishes independent outer driving from a
blocking-client resource stall. Exact-source cross-typecheck/fmt pass; native
RED execution is pending. W3 integration slices/API are recorded in the plan.
W2 fixed-source native execution is tracked separately in run44101312055;
re-review addresses both reported findings, no new Critical/Important findings.

Task W2: complete (2026-11-17, private native write/drain boundary). Fixed
sourceff1f57b passes all12 actual cli-full Windows core/root cases in
run44101312055, following native RED44097740879 and candidate GREEN44099804924.
Persistent exchanges create no drain helpers; final server drain delivers the
whole frame; writes are closed after shutdown. Native pre-entry cancellation
returns actual NOT_FOUND while the dedicated thread still holds its charge;
join, submission-return, terminal and cancel-caller gates stay independent.
Exact-source Windows cross-typecheck and fmt pass. No full native lint claim
until operational integration removes unused private adapters in W3.
Ruling: fresh review Important/Minor findings corrected; re-review ADDRESSED
for both, no new Critical/Important findings. Full #155/#165 and PR readiness
remain W3/W4 work. No wire, baseline, budgets or grace policy changes.

MODIFIED 2026-11-17 — W3a independent owner checkpoint: actual Windows
RED44102404929 source7192006 passes12 prior cases and fails only the new
synchronous-work retirement assertion, after its30s liveness watchdog; outer
cleanup returns without external watchdog. Root now retains Port outside one
scoped application thread and reaps independently of work polling; the explicit
work stack is counted with native/helper reservations. Construction, errors and
unwind remain inside that lifetime. The final-drain test additionally uses
one-byte native read regions so even short completions cannot prefetch its
unread sentinel. Exact-source cross-typecheck/fmt pass. Native GREEN, root task
review and operational facade/application integration remain pending.

Task W3a1: complete (2026-11-17, independent private native root).
Native run44103973384 at source2ea8204 passes13/13 tests without ignores;
its artifact confirms independent retirement during synchronous work and actual
retirement before error/unwind returns. Fresh root review: spec PASS, quality
APPROVED, no Critical/Important findings. Minor stack-envelope arithmetic was
corrected to count the application stack once, outside per-handle multiplication;
re-review ADDRESSED. Root-specific cross-target Clippy diagnostics are resolved,
exact-source cross-typecheck and fmt pass. Operational integration remains W3.
Ruling: private root slice approved; no claim that production #155 is solved.

MODIFIED 2026-11-17 — W3a2 native ownership-tail checkpoint. Physical retirement
now releases installed connection/request charges outside the arena mutex,
even if a cancelled task retains its logical Connection Arc. Installation
checks the abort gate under the charge mutex; ordinary request completion waits
for its native-write barrier, while final-drain charges remain until retirement.
The controlled pre-entry cancellation case checks the original semaphore
permits remain unavailable until actual retirement and become available with a
logical owner still retained. Exact-source cross-typecheck/fmt pass; native
execution and fresh review are pending. Operational daemon migration remains W3.

Task W3a2: complete (2026-11-17, private native ownership tails).
Native run44106917232 source0e4b149 passes13/13; controlled pre-entry helper
cancellation retains original permits until physical retirement, then releases
them while a logical Connection owner still exists. Fresh review PASS / APPROVED,
no findings. Exact-source cross-typecheck/fmt pass. Daemon attachment and
persistent-reply completion ordering remain operational W3 acceptance.
Ruling: ownership-tail slice approved; no production completion claim.

MODIFIED 2026-11-17 — W3a3 explicit facade checkpoint. Add public complete
transport Options/scope and a private Windows capability with cancellation-safe
accept, physical-capacity notifications and blocking read/write deadlines over
the same native operations. New native cases cover timed-out blocking reads,
shared blocking directions/final delivery and a next listener waiting for actual
arena capacity. Source cross-typecheck and lean-library Clippy pass; actual
native execution/fresh facade review and consumer migration remain pending.

MODIFIED 2026-11-17 — W3a3 review fix. Fresh review found Important: temporary
policy MutexGuards lived through blocking waits, so cloned-stream admission
could wait before starting its deadline. Snapshot each timeout before polling;
no policy guard crosses the wait. Add causal native first-read observation and
a concurrent clone test: timeout update remains available and second-read
admission expires while the first read is pending. Cross-typecheck/fmt pass;
re-review and17-case native execution pending.

Task W3a3: complete (2026-11-17, explicit native facade).
Native source d24ccf8 run44108077434 passes16/16; reviewed correction b65e8eb
run44108620658 passes17/17, including first-read-observed cloned admission.
Fresh review Important timeout-lock finding corrected; re-review ADDRESSED,
no new Critical/Important findings. Native source is cross-typechecked; full
operational Windows lint/acceptance remain W3b-d. No fallback owners/defaults.
Ruling: private facade contract approved with actual native verification.

MODIFIED 2026-11-17 — W3 operational integration checkpoint. Explicit scopes now
compose daemon/CLI/MCP/Web, clients and real fixtures; legacy interprocess
transport and dependency removed. Original permits/Activities attach before
native submission and successful ordinary/final replies use write/drain barriers.
Stop reads a complete ACK under its original overall deadline. Catalog and
state-invalidation inventory updated. Full cli-full library668/668 and Linux
all-target Clippy pass; actual migrated Windows acceptance and fresh W3 review
remain pending. The admitted stalled-reader regression now holds an actual
native cancellation caller instead of inferring pending I/O from unread bytes.

MODIFIED 2026-11-17 — W3 Linux operational verification: eight focused native
transport/client/CLI/daemon/Web integration suites pass85/85 tests (including
Stop, admission flooding, literal capability negotiation and real website
sync/readiness). cli-full library668/668, all-target Clippy, exact private native
cross-typecheck and fmt pass. Existing Windows ownership tests pass17/17 at
sourceb65e8eb; the newly migrated actual daemon cancellation gate still requires
Windows execution. Final closure/benchmarks/#165 and final review remain open.

MODIFIED 2026-11-17 — W3 native lint correction. Required Windows job
134141350866 at 185bb7c stopped before functional execution on five
platform-only Clippy findings: a test-only Duration import, two obsolete
unnecessary_wraps expectations on now-public seam methods, statement
termination and test import placement. Corrected all five without changing
transport behavior. fmt/diff checks pass; native re-execution remains pending.

MODIFIED 2026-11-17 — W3 operational native evidence. Exact source185bb7c/tree
 df080a09da51ad22a559d0bc728231afa058e10d, isolated harness466620a, native run
44112474352:18/18, independent watchdog exit0. Frozen admitted-daemon candidate
has handler_returned=true, bytes_read=1, original_available_slots=0,
original_activity=1 and native_cancel_caller_held=true. Cleanup establishes
slots=1/activity=0 after actual caller return/retirement; unread tail discarded
only as a failed exchange. Artifacts /tmp/pr164-integration-native-green.
Ruling: fresh review_164_operational at6f62a1d PASS / APPROVED, no findings;
current full/lean/native-final checks and W4 remain pending. Follow-up native
lint found a Unix-only test helper unconditionally compiled by web_lifecycle;
its module now has the same Unix cfg as its only consumer.

MODIFIED 2026-11-17 — W4a deterministic grace checkpoint. The named regression
now exercises real accept/admission/registration in both compatible and
JobRoot-enabled modes. Explicit original-deadline observation, actual admitted
worker gate and controlled grace expiry replace800files/100ms inference.
Temporary mutation removing only the expired-grace Activity recheck fails
at the intended assertion (0.04s): expired grace cut off admitted registration.
Restored implementation passes all5janitor/lifecycle tests (0.09s). Three real
subprocess cleanup tests remain. All-target cli-full Linux Clippy passes.
Fresh W4a review and final native execution remain pending.

MODIFIED 2026-11-17 — W4 native acceptance preparation. Add32actual
pending-read cancellation cycles sharing a four-handle physical arena with
persistent control traffic. Every cycle must deliver control bytes and retire
back to the two original control owners before reuse. Expand helper-free
native write/read sampling to128transfers and print diagnostic p50/p95/max;
no latency threshold changes or claimed before/after improvement. The exact
Windows core compile-only harness passes; native runtime evidence is pending.
Real subprocess cleanup3/3also passes after moving #165 coverage.

Task W4a: complete (2026-11-17, deterministic #165 regression).
Ruling: fresh review_164_grace at32b5857 PASS / APPROVED, no findings. Both real
registration paths, actual worker gate, controlled original grace expiry,
indexed/session binding and clean disconnect/idle exit are checked. Mutation
RED and restored lifecycle5/5 plus subprocess cleanup3/3pass. Native execution
is part of the still-pending W4b final-source acceptance.

MODIFIED 2026-11-17 — local full-suite environment correction. The first full
cli-full run reached install after669library tests and earlier suites passed,
then parallel install fixtures exhausted the20GiB filesystem (explicit ENOSPC).
Standard cargo clean -p docsbase --profile dev freed8.3GiB; incremental cache is
disabled for this verification run. Full suite rerun is in progress; no test,
assertion, concurrency policy, baseline or threshold was changed to hide it.

MODIFIED 2026-11-17 — W3/W4 common verification. After cache cleanup, the full
cli-full suite passes1685tests across98targets/suites;18existingignored cases
remain unchanged. Node client69/69and Website example15/15tests plus both TS
checks pass. Lean's first parallel run exposed an existing project-state
fixture inheriting another unit test's temporary global max_file_size=1.
An isolated foreign launch config deterministically reproduces the identical
0/0coverage failure; giving that fixture its own empty captured LaunchSnapshot
makes the same reproduction pass. No production config behavior changed.
Final project-state cli-full tests18/18and full lean library494/494pass;
all-target and opt-in perf_budget/artifact Clippy pass. Full cli-full functional
evidence precedes this test-fixture-only correction; native checks remain pending.

MODIFIED 2026-11-17 — W3 strengthened native RED. Isolated never-merge mutation
 e7010ff/tree9b44284 disables only the two facade ownership-tail attachments.
Run44115118167fails exactly the real admitted-daemon regression,17other native
tests pass and watchdog observes normal failure exit101. Frozen candidate:
handler_returned=true, native_cancel_caller_held=true, reader_gate_phase=1,
original_available_slots=1, original_activity=0. Cleanup joins/retire completes.
The original source185bb7c's equivalent GREEN retained slots=0/activity=1.
Artifacts /tmp/pr164-no-tail-native-red. This directly validates ownership-tail
regression sensitivity without using unread-byte counts as a retirement proof.

MODIFIED 2026-11-17 — W4b native checkpoint and review fix. Run44115522775,
source32b5857/tree8f5e03c, harness457d6b9:20/20, watchdog exit0; raw artifacts
/tmp/pr164-w4-native-green. Native128write/read diagnostic samples: p50=22us,
p95=35us, max=53us, no drain helpers; not an application-load benchmark or a
before/after speedup claim. Original admitted ownership candidate remains
slots=0/activity=1 until cancellation caller release, then slots=1/activity=0.
Ruling: fresh review_164_acceptance found Important: the new cancellation-churn
case did not causally retain work across the control exchange. Revised it to
hold actual CancelIoEx callers for BOTH native read endpoints, drop logical
streams, require all4physical records and refused extra admission before and
after the completed control reply, then release/join both callers and retire
back to2control owners. Test-only gate Drop releases/joins on errors/unwind.
Minor stale interprocess documentation corrected. Cross-typecheck passes;
revised native execution/re-review pending. The earlier20/20run does not prove
this strengthened control-overlap assertion.

MODIFIED 2026-11-17 — frame-error local ownership audit. Keep a local Arc of
Activity through explicit final error delivery as well as attaching its clone
to native retirement. Unix's no-op native attachment must not consume the only
logical Activity before the error write; Windows retention is unchanged.

MODIFIED 2026-11-17 — W3 required Windows runtime-bridge correction. Required
run44115391432at32b5857passes687library tests but the existing multi-thread
CLI bridge regression fails during future construction: the new scoped work
thread has no ambient Tokio Handle. Root now captures the caller Handle and
enters its context on that thread, while application polling uses its own
explicit runtime and native progress remains outside both. Current-thread
nested synchronous scopes are still refused. This preserves the original
construction/polling distinction; no executor, timeout or ownership policy added.
The exact existing regression is added to the isolated native selection for
GREEN evidence; required whole-suite re-execution remains pending.

MODIFIED 2026-11-17 — final review read-notification regression checkpoint.
Native run44118927632 atbd4ff22 passes21/21, including the exact existing CLI
runtime success/error/unwind/liveness case and strengthened32-cycle control
overlap; watchdog exit0, raw /tmp/pr164-context-native-green. Final whole-feature
review identified a separate late-cancellation lost wake: Operation::poll could
report a terminal result while copy_read still returned WouldBlock because a
cancellation caller retained the buffer. Terminal publication had consumed the
waker, so caller return could leave the read asleep. Extend the existing controlled
late-caller regression with a counting waker and observations before/after terminal
publication and caller release. Release/join precede assertions. Native RED and
correction remain pending; this checkpoint is not PR readiness.

MODIFIED 2026-11-17 — final review I1 native RED and correction. Isolated
run44120796760 at3401b425/treea41e9f5f fails exactly the extended late-caller
regression at its intended readiness assertion;20other cases pass and normal
watchdog-observed exit101 follows joined cancellation cleanup. Raw evidence:
/tmp/pr164-read-wake-native-red. Operation::poll now requires cancel_callers=0
alongside submission return and terminal publication under the same state lock.
Otherwise it installs the waker consumed by final caller return. New cancellation
callers already refuse returned terminal operations, preserving poll/copy fencing.
No budget, ownership or error policy changes. Native GREEN and scoped re-review
remain pending; this is the only Important final-review finding, with no Critical
or deferred Minor findings.

MODIFIED 2026-11-17 — I1 verified; W3e arrival/withdrawal follow-up. Exact native
run44122161279 ateda37b428/tree99ed4324 passes21/21, watchdog exit0; raw
/tmp/pr164-read-wake-native-green. Final reviewer I1 re-review: ADDRESSED, no
new Critical/Important findings. Required Windows run44118770344 then reaches
the real lifecycle flood fixture and fails on immediate blocking-connect busy
error231; unwinding clients also exposes accept error232 ending the daemon.
Linux/docs pass. macOS never acquired a runner (runner_id0, no steps; hosted
capacity annotation), so it is not a native test result.

W3e preserves bounded ownership while recovering known connection arrival races:
keep raw async/one-attempt connect unchanged; add an explicit original-deadline
Windows blocking wait, selected by the existing blocking client IO_TIMEOUT.
WaitNamedPipe waits on the caller thread without a helper or handle retention;
only documented busy231 retries and one deadline applies across races. The
flood fixture supplies its existing10s admission deadline. Accept withdraws and
retires only an ERROR_NO_DATA232 instance, then yields/rearms inside the same
physical arena; other errors propagate. New deterministic native regressions
first expose busy arrival and a File peer closed before ConnectNamedPipe.
The initial deadline helper intentionally still delegates the old one-attempt
path for native RED. No production settings, capacity or grace are increased.

MODIFIED 2026-11-17 — W3e native RED and correction. Run44124370836 atfa215a1
/tree106e0c14 fails exactly the new busy-arrival and pre-accept-departure cases;
21existing cases pass, watchdog normal exit101. Raw /tmp/pr164-arrival-native-red.
Implement explicit original-deadline Windows blocking admission with finite native
WaitNamedPipe, selected by existing Client IO_TIMEOUT2s. Native raw231 is classified
before one-attempt APIs map it to TimedOut. Only known abandoned232 instances
retire/rearm after yield; all original arena limits remain. The actual public
wait timeout and successful native wait are covered, including a second busy
race with monotonically decreasing remaining time and expiry before submission.
Linux lifecycle11/11 passes; exact native cross-typecheck passes (compile only).
Fresh task review, native GREEN and full required CI remain pending.

W3e local verification checkpoint: lifecycle11/11 and real CLI connection/setup
regressions4/4 pass; all-target cli-full Clippy and fmt/diff pass. Exact-source
Windows core cross-typecheck and cross-Clippy pass; the compile-only harness
suppresses only its unrelated stub unused-import reexports, never production
lints. These are not native runtime acceptance. New cases retain the original
external watchdog; departure recovery additionally bounds its joined exchange.

Task W3: complete (2026-11-17, explicit operational ownership and delivery).
The real admitted-daemon ownership mutation RED44115118167 and original native
GREEN44112474352 establish sensitivity to original permit/Activity tails.
The strengthened control-overlap case passes44118429594/44118927632. The existing
CLI ambient-runtime case passes44118927632 and the final source run below.
Common full cli-full1685/lean494, Node69/example15, native library and focused
integration evidence is recorded above. No new crate, protocol/schema/reindex,
retrieval baseline, ignore, production grace or detached native owner.
Ruling: fresh operational/root/facade task reviews approved; final I1 lost-wake
finding corrected ineda37b4, native RED44120796760 -> GREEN44122161279, scoped
re-review ADDRESSED with no new Critical/Important findings.

Task W3e: complete (2026-11-17, native connection arrival/withdrawal).
Exact sourcec7a7bcb280681c554f7d8da1d0fe98d04eb46eca/tree
bb3308f7c547a1ae8bc828cce57d8770c0de727d, isolated harness6dbec478,
run44126814441:23/23, no ignores, independent watchdog normal exit0.
Raw /tmp/pr164-arrival-native-green includes both added regressions, I1 wake,
original slots0/Activity1 while native caller held and cleanup slots1/Activity0.
The new busy/departure RED44124370836 fails exactly those2cases,21others pass.
Linux lifecycle11/11, real CLI setup/no-replay4/4, all-target Clippy/fmt and
compile-only native cross-check/Clippy pass. Existing probe/connection budgets
remain explicit; no new worker or capacity default. The fixture now waits for
busy connection admission under its explicit10s setup/probe deadline.
Ruling: fresh review_164_arrival PASS / APPROVED, no findings. Independent final
reviewer's W3e integration follow-up approved, no Critical/Important findings;
I1 stays addressed and no deferred minors remain.

Task W4b: complete (2026-11-17, source/native/review acceptance).
Final production sourcec7a7bcb run44126814441 passes23/23 with actual native
source/tree and disposable host provenance. All32cycles retain both actual
cancellation callers and4physical records across a complete control reply,
refuse extra admission, then release/join/reap to2control owners. Diagnostic
128persistent5-byte transfers: p50=22us,p95=30us,max=51us,drain_helpers=0.
W1's small actual Hello allocation98bytes versus old8388609 remains covered;
these figures are not a before/after speedup or an application capacity result.
W4a's causal real registration paths and retained subprocess cleanup remain
accepted. Tests preserve deadlines/grace/baselines/ignores and paid APIs are unused.
Ruling: final whole-feature review found I1 only, now ADDRESSED; W3e follow-up
also approved. Source/review tasks are closed; **external readiness remains
pending required docs/linux/macos/windows on the closing PR head**. Previous
macOS cancellation acquired no runner and is not a passed native check. The
closing documentation-only commit changes no production/test source from the
verifiedc7a7bcb. Mark PR ready only after its exact-head required checks pass;
merging remains a human action. No deferred minors.
