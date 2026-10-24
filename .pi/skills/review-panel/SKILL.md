---
name: review-panel
description: Run the three-model review panel (reviewer-mimo, reviewer-muse, reviewer-deepseek) over a diff, branch, module, plan or PR — strictly one reviewer at a time, aggregate in the parent, then verify the surviving claims with review-verifier before reporting. Use when asked for a multi-model / panel / cross-model review, a pre-merge review of a non-trivial change, or a second opinion from several models.
---

# Review panel (sequential, three models + verifier)

Three reviewers with **identical instructions** and **different models** give you model diversity
without argument. Diversity is only worth something if their runs are independent and their
evidence is real, so this skill is about the orchestration discipline: one at a time, identical
brief, no leakage, aggregate in the parent, then let an adversarial verifier decide which claims
survive.

| Order | Agent | Model | Effort | Role |
|---|---|---|---|---|
| 1 | `reviewer-mimo` | `opencode-go/mimo-v2.6-flash` | n/a — `thinking: off` sends no `reasoning_effort`, provider default | review |
| 2 | `reviewer-muse` | `opencode-go/muse-spark-1.3-contributor` | `xhigh` | review |
| 3 | `reviewer-deepseek` | `opencode-go/deepseek-v4.1-flash` | `max` | review |
| 4 | `review-verifier` | `opencode-go/gpt-6-luna` | `high` | verify the aggregated claims |

Order is fixed cheapest-first for reproducibility. Do not reorder per run; comparable order is what
makes "model A missed this" a meaningful statement later. The verifier is a fourth, unrelated model
on purpose: a model that reviewed the change cannot referee its own findings.

Order is fixed cheapest-first for reproducibility. Do not reorder per run; comparable order is what
makes "model A missed this" a meaningful statement later.

## Why sequential

Every reviewer may build, run `cargo test`, `cargo clippy`, benchmarks and the daemon. Parallel
children share `target/`, the lock file, unix sockets, fixed ports, temp dirs and the SQLite store,
so parallel runs produce lock contention, false test failures and misleading "reproduced" claims.
Serialization is a correctness requirement here, not a speed preference. Say so if someone asks for
a parallel fan-out.

## Step 0 — Preflight

1. Require delegation: call `subagents_enable({})` first if `subagent` is not yet available.
2. Confirm the three agents resolve: `subagent({ action: "list" })` (project scope wins over user
   scope; if `.pi/agents/*.md` is missing, say so and stop instead of substituting `reviewer`).
3. Pin the target exactly: `git rev-parse --short HEAD`, `git status --short`, the base ref, and
   `git diff --stat <base>...HEAD`. A dirty tree must be either committed, stashed by the human, or
   declared as part of the target — never silently included.
4. Record the starting ref before any checkout (`git rev-parse --abbrev-ref HEAD`) and restore it
   when the panel finishes, or say in the report which ref the tree was left on. Switching branches
   for the review is an orchestrator side effect the human did not ask for.
5. Record the environment constraints you will pass on: MSRV toolchain, `CARGO_BUILD_JOBS`, offline
   build, no network, `INSTA_UPDATE` unset, no test that needs a live API key — and the exact
   `export` line reviewers must run (a toolchain installed but not on `PATH` is invisible to them).
6. **Warm the build once, in the orchestrator.** Run the project's build or test-compile step before
   the first reviewer, so three sequential runs reuse one `target/` instead of each paying a cold
   build. Record the wall time and the free disk; pass both to the reviewers as facts.
7. Decide the scope. Whole-repo reviews are expensive; scope to the diff, a directory, or one
   subsystem. Ladder for large ranges: >2000 changed lines → narrow to the 3–5 riskiest modules and
   say so in the report; >8000 → split by subsystem and run separate panels. Budget roughly 15–40
   minutes per reviewer for a medium diff.

## Step 1 — Write one brief, reuse it verbatim

All three reviewers get the same task text. Differing briefs destroy comparability, and findings you
put in a later brief contaminate its independence.

```
Review target: <commit range | branch> in <repo path>.
Base: <ref>. Head: <sha>. Scope: <paths, if narrowed>.
Mode: diff review — report only issues caused or made reachable by this range, labelled [new].

Intent: <one paragraph: what the change is supposed to do, in user terms>.
Acceptance criteria: <the 3-6 observable statements that make this change correct>.
Spec / PR: <docs/specs/<name>/requirements.md#SC-x, PR description, issue link — paths the
 reviewer should read before judging intent>.
Product model (facts, not hints): <how identity, data or config actually arrive in this system
 today, and which component is authoritative — so the reviewer can judge the premise>.
Deliberate decisions (not findings): <known tradeoffs, documented exceptions, deferred work>.
Measurement (if any): <counter, metric, log line, test or benchmark that should prove the new
 path is actually taken — omit if the change has none>.
Read repository instructions (AGENTS.md, docs/constitution.md) as binding criteria.

You may build, test, lint and benchmark freely. Do not modify project file contents, do not
regenerate snapshots or baselines, and do not run git mutations. Stop anything you start.

Priority: correct behaviour, race conditions and security first, then data integrity, error
paths, performance, contracts, tests, and stale comments/docs/specs.

Return exactly the report format from your instructions: verdict, P0/P1/P2 findings with
`path:line` evidence, verified good, not verified, and commands for the supervisor.
Budget: <N> minutes. Do not ask questions — state assumptions and proceed.
```

What belongs in the brief and what does not:
- **Belongs:** target refs, intent, acceptance criteria, spec/PR paths, the deliberate-decision
  list, scope, budget. Without intent the reviewer can only check internal consistency — it cannot
  tell you the feature does the wrong thing very neatly.
- **Does not belong:** your own suspicions ("check the lock around line 120"), earlier reviewers'
  findings, a paste of the diff itself. Reviewers can run `git diff`, read files and grep; a pasted
  diff wastes their context and hides the surrounding code that proves or kills a finding.
- List changed paths when the range is large (>30 files), so nobody burns the budget on a tree walk.
- Name intentional deviations explicitly. Unstated deliberate choices come back as findings and cost
  a verification round on your side.

**Brief self-check before you send it** (a wrong fact here poisons all three runs at once):

- every claim in `Product model` was verified against the base ref, not against the branch's own
  ledger or changelog (`git show <base>:<path>`, `git grep <symbol> <base> -- <path>`);
- every `Spec / PR` path exists at the reviewed ref (`git show <head>:<path> >/dev/null`);
- `Base`/`Head` resolve, and the scope paths are inside the diff;
- no hypothesis, no "check X near line Y", no earlier reviewer's findings;
- the environment block names the toolchain, the `export` line, the build state and the disk budget.

If a reviewer later reports that a brief fact is wrong, treat that as a panel success: verify it,
correct the brief, and record the correction in the report.

Keep the brief free of your own hypotheses, of earlier reviewer output, and of "please check X near
line Y" hints unless the human explicitly asked for a targeted probe (then note in the final report
that the panel was steered).

## Step 2 — Run the panel, one at a time

Preferred form — a sequential workflow, so ordering is enforced by the script:

```js workflow
const brief = "<the brief from step 1>";
const mimo = await runs.run("mimo", { agent: "reviewer-mimo", task: brief });
const muse = await runs.run("muse", { agent: "reviewer-muse", task: brief });
const deepseek = await runs.run("deepseek", { agent: "reviewer-deepseek", task: brief });
return [mimo.output, muse.output, deepseek.output];
```

Manual equivalent, one call per message so each run finishes before the next starts:

```text
subagent({ agent: "reviewer-mimo", task: "<brief>", async: false, options: { timeoutMs: 2400000 } })
```

After each run, before launching the next:

- read that reviewer's verdict and findings into your own notes;
- run `git status --short` **as its own call** and confirm the tree is unchanged — a reviewer that
  edited, formatted or regenerated something is a process failure: stop the panel, restore the tree
  with the human, and report the run as invalid rather than trusting its findings;
- check for leftover processes, daemons, sockets or lock files it started, and clear them;
- do not feed its findings into the next brief.

A P0 in the first run does not cancel the panel: independence is the whole point, and later runs may
find a second root cause or refute the first. It only shortens the report when the human explicitly
asks for blockers only.

Expect truncation: a full reviewer report usually exceeds the inline result budget and is written to
an artifact file whose path the run prints. Read the artifact before aggregating — the inline text is
a summary, and the P0/P1 detail plus `Verify` commands live in the file. Keep the three artifact paths
in your notes; they are the evidence trail for the report.

## Step 3 — Failure and budget handling

- A launch error, provider error or timeout: retry that reviewer **once**. If it fails again, record
  it as `missing` and continue with the rest.
- Fewer than two completed reviews means the panel result is `inconclusive` — say that instead of
  presenting a single model's opinion as a panel verdict.
- If the wall-clock or cost budget is tight, drop to two reviewers (mimo + deepseek) and state the
  reduced panel in the report. Never silently drop one. When the operator cares about spend, pass
  `options.usageBudget` (soft/hard tokens or costUsd) so the panel stops before a runaway run.
- Trivial change (docs-only, typo, one-line config): skip the panel, use one reviewer, and say so.

## Step 4 — Aggregate (in the parent session)

1. **Normalize.** Map each finding to its `Root cause` key (`path::symbol::class`) when the reviewer
   supplied one, otherwise to `file + enclosing symbol + line window`. Merge findings that describe
   the same root cause even when the wording, severity or line differs.
2. **Vote.** Build a per-finding tally: which reviewers raised it, with which severity.
   - 3/3 or 2/3 agreement → high confidence.
   - 1/3 → candidate; keep it, but it must be verified by you before it reaches the report.
   - Raised then explicitly refuted by another reviewer → list as `disputed` with both arguments.
   - Different severities for the same root cause → verify, then take the stricter label if its
     evidence holds; otherwise record the stricter one as disputed rather than silently downgrading.
3. **Verify before you repeat.** For every distinct claim, open the cited file and read the cited
   lines yourself, and run the finding's `Verify` command where it supplied one. Reviewer claims are
   hypotheses; a claim you did not check is not a finding. Where a command is needed, run it or mark
   it `needs runtime confirmation`. For caching, indexing or batching changes, open the call site
   that produces the result yourself and confirm which path runs — a component can be wired on the
   write side and bypassed on the read side while every unit test stays green.
4. **Fact disputes are not judgment disputes.** If one reviewer reports "clippy is red" and another
   "clippy is clean", that is a measurable claim: run the command yourself, name the toolchain and
   flags, and report the measured result. Never average two contradictory facts into a note.
5. **Calibrate severity.** When reviewers disagree, the stricter reading wins for P0 *candidates*
   (they must be checked), and the better-evidenced reading wins for the final label. One verified
   P0 is enough to block — unanimity is not required to block, only to relax.
6. **Cap the P2 flood.** Carry every P0/P1 into the report, but at most six P2 (highest maintenance or
   operational cost first); list the rest by reference under `Also noticed`. A report where twelve
   P2 bury three P1 is a failed aggregation, not a thorough one.
7. **Watch for shared blind spots and shared noise.** All three flagging the same thing is not proof
   (a misleading comment can fool three models identically) — verify it like any other item. A
   finding everyone gets wrong is a hint that the prompt or the code's comment is misleading; note
   it, don't ship it.
8. **Keep scope objections separate from defects.** "The code is fine but nothing calls it", "this
   fixes the symptom, not the bug", "this should not have been built" and "half the layers are
   missing" are not P0/P1 line findings — they go in `Scope & necessity` with their own evidence.
   The same goes for a false premise: a perfectly built feature for a problem this product does not
   have (self-registration on a system whose accounts all come from an identity integration) is a
   scope objection, not a style note. Never let one disappear because the diff itself reads
   correctly, and never inflate one into a block without showing what is unreachable, still broken,
   or contradicted by a documented invariant. The decision belongs to the human.
   When two or three reviewers independently conclude a subsystem is unreachable or unnecessary,
   treat that as a re-scope signal, not as noise.

## Step 5 — Verify the surviving claims (verifier subagent)

The parent aggregates; the verifier adjudicates. Do not hand the verifier the three raw reports — it
would become a fourth reviewer with worse context, and aggregation decisions (dedupe, votes,
severity, scope) belong to the session that owns the final verdict. Hand it a **clean numbered claim
list** instead.

Who verifies what:

- every P0 and P1 goes to the verifier;
- every P2 you intend to publish (max six) goes too;
- findings the parent already proved with a sub-30-second command may be marked `verified by parent`
  and skipped, but say so in the report;
- if the panel returned nothing, there is nothing to verify — skip this step.

Verifier brief (same target refs as the panel, plus the claims):

```
Verify the claims below against <repo> at <head SHA>. Base for context: <base ref>.
Acceptance criteria they refer to: <3-6 lines from the panel brief>.
Environment: <toolchain, export line, build state, disk budget>.

For each claim return exactly one status: VERIFIED / PARTIAL / REFUTED / STALE / UNVERIFIABLE,
with the quote, the command output, or the precise reason you could not obtain one. Correct the
location and the severity when the claim is directionally right but wrong in detail. Do not add
findings (one line in Incidental at most), do not edit, do not re-review the diff, do not re-rank.

C1 | P0 | `path::symbol::class` | path:line (symbol)
Problem: ...
Evidence: ...
Impact: ...
Verify: `<command>`   (no reviewer names, no vote counts)

C2 | ...
```

Give the verifier the head **SHA**, not only a branch name: the checkout may be a local branch
(`review/<x>`) while the panel briefed `docs/<x>`, and an unqualified name then fails to resolve.
When the verifier's header disagrees with the severity you sent, trust its `Correction` line and not
the header; our scale has P0/P1/P2, so a suggested "below P2" becomes an unranked `Also noticed`
note rather than a new level.

**Anonymize the claims.** Send no reviewer model names, no vote counts and no "3/3 agreed" markers:
attribution invites sympathy for a model family or deference to consensus, and both defeat the point
of an adversarial pass. The parent keeps the votes in its own notes and reconciles them with the
verifier's statuses afterwards. If the review was steered (a targeted probe at a human's request), do
say so — that is a fact about the brief, not about the finder.

Run it once, after the panel, with the same warm `target/`. Handle its statuses like this:

| Status | What the report says |
|---|---|
| VERIFIED | publish as claimed, with the verifier's evidence |
| PARTIAL | publish with the corrected severity/location, marked `corrected` |
| REFUTED | move to `Disputed` with the refutation; do not publish as a finding |
| STALE | drop, and note that the claim described code that no longer exists |
| UNVERIFIABLE | publish as `needs runtime confirmation` with the exact command, or drop if it is P2 |

If the verifier's verdict conflicts with a check you ran yourself, your direct evidence wins — but
record the conflict and both evidences in `Disputed` rather than silently overriding.

## Step 6 — Report

```markdown
## Review panel
- Target: <range/scope>, base <ref> → head <sha>
- Panel: reviewer-mimo ✔, reviewer-muse ✔, reviewer-deepseek ✘ (timeout)
- Verdict: BLOCK | OK WITH NOTES | OK | INCONCLUSIVE
- Scope: right change for the stated problem | needs re-scoping | should not be built (this way)
- Cost: <wall time per run if available>; scope reduction applied: <none | narrowed to X>

### Findings (deduplicated, verified)
| # | Sev | Root cause | Location | Finding | Votes | Verified |
|---|-----|-----------|----------|---------|-----------|----------|
| 1 | P0  | `src/daemon/x.rs::start::lost-update` | src/daemon/x.rs:120 | lost update on index manifest | 1/3 | verifier: VERIFIED |

<full detail per finding: problem, evidence, impact, smallest fix>

Pre-existing vs new: <which rows are [pre-existing] and therefore do not block this change>

### Scope & necessity
- <unreachable / half-wired / symptom-only / premise contradicted by the product's documented model
  / duplicate mechanism / out of documented scope, with the entry point or invariant you traced and
  the alternative>

### Disputed / unverified
- <claim> — raised by X, refuted by Y because ...; needs <command> to settle.

### Panel-only notes
- <model-specific miss or false positive worth knowing>
```

Rules: the human gets your verified synthesis, never three raw transcripts. If the panel is clean,
say `No issues found.` and stop. Never merge, never push, never "fix while reviewing" — prepare the
report and stop. If the review belongs to a spec task and the project process requires it, append one
panel entry (verdict, verified findings, commands still owed) to `docs/specs/<name>/progress.md`; do
not rewrite the ledger's earlier entries. Restore the starting ref if you switched branches, or say
which ref the tree is on.

## Anti-patterns

- Launching the three reviewers in parallel because it is faster — see *Why sequential*.
- Passing reviewer 1's findings into reviewer 2's brief (destroys independence and cross-model
  agreement).
- Relaying a reviewer's P0 without reading the code it points at.
- Letting a reviewer "quickly fix" something it found: the reviewer that finds an issue must stay
  independent of the change that resolves it.
- Regenerating golden baselines, snapshots or fixtures so the panel turns green.
- Treating 3/3 agreement as proof, or 1/3 as noise, without verification.
- Declaring a panel clean without anyone tracing the feature from an entry point — three models can
  miss "nothing calls this" identically, and no amount of agreement fixes that.
- Reporting a scope objection as an ordinary P2 note, or dropping it because the code is correct.
- Sending a brief fact you did not verify against the base ref (one wrong fact misdirects all three
  runs and costs a re-run).
- Letting a dozen P2s bury the P0/P1 that actually decide the merge.
- Paying for a cold build three times because the orchestrator did not warm `target/` first.
- Running the post-run `git status` check in the same batch as the next launch instead of before it.
- Giving the verifier the three raw reports and asking it to aggregate — that is the parent's job,
  and it turns verification into a fourth review with worse context.
- Sending vote counts, reviewer model names or "3/3 agreed" to the verifier (confirmation bias and
  model-family sympathy), or letting a single-vote claim skip verification because it is lonely.
- Accepting a verifier PARTIAL as "fine as is": a PARTIAL is a correction to apply, not a pass.
- Using the verifier to add new findings; anything outside the claim list goes to `Incidental` and is
  the parent's call.
