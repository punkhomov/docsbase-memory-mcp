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
