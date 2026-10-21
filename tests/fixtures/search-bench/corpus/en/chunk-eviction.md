# Chunk Eviction

## When entries are evicted

The cache evicts entries whose vectors are stale.
Evicted chunks are deleted, not archived.

## Eviction policy

Configure the eviction threshold per project.
Entries evicted early still count toward coverage.
