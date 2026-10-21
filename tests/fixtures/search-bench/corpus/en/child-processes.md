# Child Processes

## Supervision

The supervisor tracks every child process it spawns.
A child that exits unexpectedly is restarted with backoff.

## Per-child State

Each child keeps its own shard index on disk.
The supervisor never shares one index between child processes.

## Restart Policy

Restart counts are kept per child, not globally.
Three crashes in a minute park the child for manual review.

## Graceful Shutdown

On shutdown the supervisor asks each child to drain first.
A child ignoring the signal is killed after ten seconds.

## Child Logging

Every child writes to its own log file.
The supervisor prefixes each line with the child id.

## Orphaned Processes

A child whose parent died is adopted by init.
Adopted processes keep running with their old index.

## Health Checks

The supervisor probes each child every five seconds.
A child failing three probes in a row is restarted.

## Limits

No more than one hundred child processes per supervisor.
Beyond that, split the fleet into several supervisors.
