# Atomic replace

The updater performs an atomic replace of the binary: the new file is written
next to the old one and renamed over it.

The atomic replace never leaves a partially written executable behind.
