# Nightly Runs

## Schedule

The nightly job runs at three in the morning.
Each run processes everything queued since the previous run.

## Where Logs Go

When the run finishes, its logs go to cold storage.
Logs of a failed run go to the incident bucket instead.

## Report Writing

Every run writes a summary report on completion.
The run writes the report even when it itself failed.

## Duration

A typical run takes forty minutes end to end.
Runs longer than two hours are killed and investigated.

## Retries

A failed run is retried once, never more.
The retry runs with the same parameters as the original run.

## Notifications

The on-call engineer is paged only when a run fails twice.
A single failed run just sends mail.

## Retention

Run reports are kept for ninety days.
Older reports are deleted automatically.

## Manual Runs

An operator can trigger a run outside the schedule.
Manual runs skip the notification step.
