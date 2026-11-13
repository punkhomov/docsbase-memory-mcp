import { posix } from 'node:path';
import { DocsbaseError, type ClientOptions } from './types.ts';

function integer(value: unknown, minimum: number, maximum = Number.MAX_SAFE_INTEGER): boolean {
  return (
    typeof value === 'number' && Number.isSafeInteger(value) && value >= minimum && value <= maximum
  );
}
/** Single owner of validation; all policy fields are required, with no transport defaults. */
export function resolveClientOptions(
  input: ClientOptions,
  platform: NodeJS.Platform = process.platform,
): Readonly<ClientOptions> {
  if (
    !input ||
    typeof input.endpoint !== 'string' ||
    input.endpoint.includes('\0') ||
    !(platform === 'win32'
      ? input.endpoint.startsWith('\\\\.\\pipe\\') && input.endpoint.length > '\\\\.\\pipe\\'.length
      : posix.isAbsolute(input.endpoint)) ||
    !integer(input.projectId, 1) ||
    typeof input.expectedBuildId !== 'string' ||
    input.expectedBuildId.length === 0 ||
    !integer(input.expectedSchemaVersion, 1, 0xffffffff) ||
    !input.settings ||
    !integer(input.settings.searchConnections, 1) ||
    !integer(input.settings.controlConnections, 1) ||
    !integer(input.settings.queueLimit, 0) ||
    // Node timers use signed 32-bit millisecond delays, not an operational default.
    !integer(input.settings.requestTimeoutMs, 1, 2 ** 31 - 1)
  ) {
    throw new DocsbaseError('validation', 'invalid or incomplete docsbase client configuration');
  }
  return Object.freeze({
    endpoint: input.endpoint,
    projectId: input.projectId,
    expectedBuildId: input.expectedBuildId,
    expectedSchemaVersion: input.expectedSchemaVersion,
    settings: Object.freeze({ ...input.settings }),
  });
}
