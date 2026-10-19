# Windows Named Pipe

## Path

The daemon listens on `\\.\pipe\docsbase` on Windows.
Clients connect to the pipe name from the config.

Named pipes need no filesystem cleanup.
