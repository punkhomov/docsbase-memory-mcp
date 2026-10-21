# Frame Budget

## Size cap

`MAX_FRAME_BYTES` caps one IPC frame at one megabyte by default.
Frames larger than `MAX_FRAME_BYTES` are rejected.

## Window

`FRAME_BUDGET_MS` bounds the batch window in milliseconds.
Exceeding `FRAME_BUDGET_MS` drops the batch.
