# Watch Batch Limit

## Hard cap

`WATCH_BATCH_LIMIT` is the absolute ceiling for one batch.
It cannot exceed the operating system watch count.

The limit applies after debouncing, not before.
