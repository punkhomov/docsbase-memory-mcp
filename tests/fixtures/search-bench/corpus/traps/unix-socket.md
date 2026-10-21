# Unix Socket

## Path

The daemon listens on `docsbase.sock` in the data directory.
Clients connect to the socket path from the config.

Remove a stale socket file before rebinding.
