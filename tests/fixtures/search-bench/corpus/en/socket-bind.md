# Socket Bind

## Binding the daemon socket

Bind the daemon socket to a path from the config.
The daemon listens on the bound socket after start.

## Verification

Send the daemon a status command over the socket.
A bound socket rejects unknown peers.
