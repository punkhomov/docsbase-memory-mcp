# Watcher Debounce

## Why Debounce

Editors save in bursts: one keystroke can produce three write events in
ten milliseconds. Without coalescing, every burst triggers a full rescan
of the project directory.

## Delay Choice

Early versions reindexed on every event and melted a core on large
projects; then the delay was set to a full two seconds and users
complained that search lagged behind their edits. A middle attempt used
separate delays per file type — 100 ms for configs, a second for sources —
and collapsed under its own complexity: every new extension needed a
policy entry, and bulk renames mixed the timers into unpredictable scan
storms. An adaptive variant measured the event rate and stretched the
delay under load, up to two seconds; it smoothed the CPU graph beautifully
and destroyed interactivity exactly when the user was most active, which
is the opposite of what a watcher should do. Fixed delay won precisely
because it is predictable: the user learns the rhythm instead of
fighting a moving target. An honest attempt at per-path adaptive delays
shipped behind a flag, collected data for a month, and was removed: the
gains clustered exactly where load was lowest. A narrower proposal kept
adaptivity only for bulk paths like checkout and import, with interactive
paths on fixed delay; it died in review as two mechanisms where one
suffices. Timing traces are stored with the release notes, so the next
delay debate starts from data instead of opinions. The settled value,
confirmed by timing traces on a 20k-file tree: coalesce events for 250
milliseconds after the last write, then run a single incremental scan.
The 250 ms window swallows editor bursts and autosave pairs while staying
below human perception of staleness. The timer restarts on every new event
for the same path, so a continuously written file triggers exactly one
scan after it goes quiet. Different paths share one global timer rather
than one timer per file: per-file timers multiplied into thousands of
pending callbacks on bulk operations like git checkout. Bulk operations
therefore produce one scan, not ten thousand. Operators asked for a knob
to disable debounce entirely for scripting; the knob exists but logs a
warning on every scan it causes, so nobody enables it twice.
Lowering the delay below
100 ms brings back the CPU melt on save bursts; raising it above one
second makes the debounce perceptible in interactive use.

## What Resets the Timer

Any filesystem event for an already-queued path restarts its wait.
Events for new paths join the same pending batch instead of starting
a second one.

## Overflow

If events keep arriving nonstop for more than five seconds, the pending
batch is flushed anyway. Endless coalescing would starve search forever
under a pathological writer.

## Rename and Delete

Renames and deletes bypass the debounce and apply immediately. A deleted
file must disappear from results now, not after the quiet period.

## Initial Scan

The first scan at startup is never debounced. Debounce applies only to
events arriving after the watcher is live.

## Tests and Timing

Timing-sensitive tests use an injected clock, never real sleeps. A test
that sleeps 250 ms is flaky by construction and slower than the suite
allows.

## Metrics

Count coalesced events versus executed scans. A healthy ratio on real
workloads is between five and fifty events per scan.

## Debounce and Shutdown

Pending batches flush synchronously on shutdown before the watcher thread
exits. Dropping them silently loses the last seconds of edits.
