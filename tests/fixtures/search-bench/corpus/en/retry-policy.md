# Retry Policy

## Retried batches

Failed batches are retried with exponential backoff.
Batches retried more than three times are dropped.

## Backoff schedule

The first retry waits one hundred milliseconds.
Each retry doubles the previous delay.
