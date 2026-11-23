# Installer SIGKILL: bounded follow-up, 2026-11-24

Baseline069ff64. #113 remains an unexplained historical failure, not a confirmed
code-signing or OOM diagnosis. Prior #121 already replaced in-place launcher
copying with a fresh file. Do not repeat the prior150 synthetic --version trials.

## Evidence examined

- Original job130056729389 in run42880945651 remains downloadable. It records
  install_owned_artifacts, Unix status9 and empty stdout/stderr, without child
  PID, precise install invocation or Apple termination namespace.
- Queried the first100 failed workflow runs filtered created after2026-11-11;
  28 were ci runs. Examined latest-attempt macOS job steps for all28, then all
  four jobs failing their functional suite:131268412743 (registry locking),
  131774276461 (Web lifecycle),133889278673 and133918982289 (cleanup registration).
  None reproduces installer SIGKILL. This bounded/latest-attempt sample is not an
  exhaustive history or evidence that #113 is fixed.
- Post-#168 main run45270160598 has docs/Linux/Windows success and macOS canceled
  with no steps. It supplies no installer execution evidence. Do not classify
  that cancellation as a SIGKILL or silently rerun it until green.

## Primary sources and inference limits

[Apple: Updating Mac Software](https://developer.apple.com/documentation/security/updating-mac-software)
explains cached signatures and recommends replacing signed files using a new file.
Our existing stage_launcher follows that recommendation. The guidance explains a
possible mechanism; it does not attribute the original failure to that mechanism.

[Apple: Acquiring crash reports and diagnostic logs](https://developer.apple.com/documentation/xcode/acquiring-crash-reports-and-diagnostic-logs)
explains obtaining .ips/.crash reports and correlating console evidence with the
failure time. A matching termination namespace/reason is missing in our old log.

## Small observability improvement and stopping rule

The installer test helper retains the actual child's PID, executable path, test
name and final status. Consecutive install invocations produce separate ordered
PID records. Existing ETXTBSY spawn-only retry behavior is preserved; a spawned
process killed by SIGKILL is returned as failure with unchanged output assertions.

On macOS CI failure, a best-effort collector extracts selected fields from recent
matching-name/PID .ips candidates into existing diagnostics. Inputs are bounded;
no broad system log, environment dump, signature repair or failure retry. PID reuse
is possible, so path/time must be corroborated before cause attribution. A missing,
late or inaccessible report remains missing evidence. Synthetic collector tests
are not native proof of report generation.

One required native PR run validates the changed test helper. If that run provides
no installer failure/report, stop: #113 remains open awaiting a real failing PID
with corresponding path/time and OS termination evidence. Do not tune production
code, add SIGKILL retries or weaken native assertions to close the issue.

The final native run URL, exact HEAD and outcome are recorded in the delivering
PR's validation receipt. That receipt completes this report's acceptance evidence;
the source record above fixes the investigation scope before the run.
