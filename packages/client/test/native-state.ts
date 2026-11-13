import assert from 'node:assert/strict';

// Fixture only: daemon.json owns the endpoint after cache creation/startup.
export function daemonEndpoint(
  state: { socket?: unknown },
  platform: NodeJS.Platform = process.platform,
): string {
  assert.ok(
    typeof state.socket === 'string' && state.socket.length > 0,
    'daemon state must include a nonempty socket',
  );
  // Rust serializes Endpoint::Pipe as its bare name; Node uses the pipe namespace.
  return platform === 'win32' ? `\\\\.\\pipe\\${state.socket}` : state.socket;
}
