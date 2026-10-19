# Quotas and Limits

## Token Bucket

Every rate limit is a token bucket: capacity sets the burst size, refill
rate sets the sustained throughput. A request takes one token; no tokens
means wait or reject, never a queue that grows without bound.

## Per-Key Buckets

Limits apply per API key, not globally. One abusive key exhausts only its
own bucket; the shared default bucket stays untouched for everyone else.

## The Refill Math

Buckets refill continuously, not once per second, and this exact choice
fixed a long-standing burst of 429s at every second boundary. The old
discrete refill released the full per-second quota in one instant, so
clients synchronized by retry timers all passed at 12:00:00.500 and all
failed at 12:00:00.900. A variant with per-client phase offsets was
considered and rejected: offsets only hide the synchronization until
clients converge on the same retry schedule again. The incident that
settled the debate lasted eleven minutes: three synchronized clients
emptied the discrete bucket in the first 50 ms of every second and
starved interactive traffic eleven times in a row before anyone looked
at the refill code. The fix took one line; the postmortem took eleven
pages, mostly about why nobody graphed per-second admission before.
A follow-up audit found two more services with the same discrete refill,
both quietly 429ing their heaviest users at every second boundary; they
were migrated the same week. The shared library now exposes only the
continuous variant, and the discrete one was deleted so nobody reverts
by accident. Client SDKs now document the refill model next to the retry
policy: callers that understand continuous refill stop clustering retries
on second boundaries on their own.
Continuous refill spreads admissions evenly:
each millisecond adds rate/1000 tokens, and the bucket never holds more
than its capacity no matter how long it idles. When checking a request,
compare the fractional balance against one full token and deduct exactly
one; rounding the balance down on every check leaks throughput under
load, which took a week of profiling to find. Clocks go backwards on NTP
adjustments, so the refill computation clamps negative elapsed time to
zero instead of removing tokens for time travel. Operators also asked
whether buckets survive restarts; the answer is no by design, and the
capacity cap bounds the one-time burst after a crash to documented size.

## Burst vs Sustained

Capacity ten with refill one per second allows ten instant requests, then
one per second. Document both numbers: users plan retries around the
burst and steady load around the rate.

## Headers

Every response carries the remaining quota and the reset time. Clients
that ignore the headers and retry blindly earn their 429s.

## Over Quota

Past the limit the server answers 429 with Retry-After. It never queues
the request silently: silent queues turn quota problems into latency
problems that are harder to diagnose.

## Quota Storage

Bucket state lives in memory with periodic snapshots. Losing the snapshot
on crash only grants a one-time burst, which the capacity cap bounds.

## Testing Limits

Tests use buckets of size two with instant refill disabled. Any test that
waits on a real refill timer is slow and flaky by design.

## Raising Limits

Quota increases go through review: doubling a limit doubles the worst
case load on the backend. No self-service increases above the preset
tiers.
