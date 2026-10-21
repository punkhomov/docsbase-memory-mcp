# Watch Policy

## Batch size

`WATCH_BATCH_SIZE` caps the number of paths per batch.
Batches larger than `WATCH_BATCH_SIZE` are split.

## Debounce

`WATCH_DEBOUNCE_MS` delays the batch after the last event.
Set `WATCH_DEBOUNCE_MS` to zero to disable the delay.
