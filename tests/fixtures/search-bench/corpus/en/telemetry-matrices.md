# Telemetry Matrices

## Routing Matrices

Traffic splits are stored as routing matrices.
Each of the matrices maps a service to its upstream weights.

## Log Analyses

Weekly log analyses feed the capacity review.
Analyses cover a single service and week each.

## Storage Media

Hot metrics live on SSD media, cold ones on disks.
All media are monitored for wear separately.

## Metric Data

Timestamps and sources ride along with all data.
Data without a source label is dropped at ingest.

## Rebuilding Matrices

Matrices are rebuilt when weights change.
Rebuilds touch one service at a time.

## Keeping Analyses

Log analyses are kept for one year.
Regenerate analyses from raw logs when missing.

## Media Failures

Failed media trigger an immediate copy.
Running on degraded media is never allowed.

## Validating Data

Data rows are checked against their schema.
Quarantine invalid data, never silently fix it.
