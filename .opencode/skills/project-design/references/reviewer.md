# Reviewer prompts (machine mode)

Fresh review is the quality gate in `--machine` execution. The reviewer never built the
change and never inherits the implementer's context. Pass **file paths**, never pasted diff
text. Never instruct a reviewer to ignore, downgrade, or not flag something.

## Task review

Dispatch to a fresh subagent after a task's completion contract is met.

```
You are a fresh reviewer. You did not write this code. Review one task.

Inputs:
- Task brief: <path to the task section/brief>
- Review package: <path to diff file: git log --oneline, diff --stat, diff -U10>
- Global constraints (binding, copied verbatim): <constraints>
- Spec: <path to requirements.md>

Method:
1. Read the brief. List every requirement and expected value it names.
2. Read the review package. Check each requirement against the actual diff.
3. Check quality: test hygiene (do tests assert real behavior?), YAGNI, error handling,
   names/signatures consistent with the brief and with earlier tasks, no unrelated edits.
4. Do not re-run tests the implementer already ran; the report carries that evidence.

Return exactly:
- Spec compliance: PASS / FAIL — for FAIL list each unmet requirement.
- Quality: APPROVED / CHANGES — list findings.
- Findings: each with severity Critical / Important / Minor, file:line, and one line why.
- Cannot verify from diff: requirements that live in unchanged code or span tasks.
```

## Re-review

Dispatch after a fix round. Scope is the fix diff only.

```
You are re-reviewing a fix for previously reported findings.

Inputs:
- Original findings: <list verbatim>
- Fix diff: <path>

Return, for each original finding:
- ADDRESSED / NOT ADDRESSED with file:line evidence.
Then: new breakage introduced by this fix diff only — Critical/Important, or none.
Do not reopen closed findings, do not review untouched code, do not add new scope.
```

## Final review

Dispatch once, after the last task, on the most capable available model.

```
You are the final reviewer for this feature. You did not write this code.

Inputs:
- Review package: <whole-branch diff path>
- Spec: <requirements.md>
- Design: <design.md>
- Tasks: <tasks.md>
- Deferred minors from the ledger: <list>

Method:
1. Map every FR-* and SC-* to evidence in the diff; flag unmet or unverifiable ones.
2. Check every task is accounted for; flag drift between code and design interfaces.
3. Triage the deferred minors: which must be fixed before merge, which can ship.
4. Judge failure modes a real user would hit, even if the spec is silent on them.

Return:
- Verdict: converged / partial / failed.
- Unmet requirements with evidence.
- Critical / Important findings (one fix pass will follow).
- Minor findings to defer.
```
