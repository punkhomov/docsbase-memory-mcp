/** The sole application boundary for complete operator policy. No defaults. */
import { isIP } from 'node:net';
import { constants as bufferLimits } from 'node:buffer';
import { DocsbaseClient, type ClientOptions } from '@docsbase/client';

export interface HttpPolicy {
  readonly host: string;
  readonly port: number;
  readonly maxConnections: number;
  readonly searchActiveRequests: number;
  readonly searchPendingRequests: number;
  readonly controlActiveRequests: number;
  readonly controlPendingRequests: number;
  readonly maxHeaderBytes: number;
  readonly maxHeadersCount: number;
  readonly maxBodyBytes: number;
  readonly maxQueryBytes: number;
  readonly maxResultBytes: number;
  readonly maxHits: number;
  readonly requestTimeoutMs: number;
  readonly headersTimeoutMs: number;
  readonly headerCheckIntervalMs: number;
  readonly idleTimeoutMs: number;
  readonly keepAliveTimeoutMs: number;
  readonly maxRequestsPerSocket: number;
}
export interface WebsiteConfig {
  readonly http: Readonly<HttpPolicy>;
  readonly client: DocsbaseClient;
}
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function integer(value: unknown, min: number, max = 2 ** 31 - 1): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= min && value <= max;
}
const required = [
  'host',
  'port',
  'maxConnections',
  'searchActiveRequests',
  'searchPendingRequests',
  'controlActiveRequests',
  'controlPendingRequests',
  'maxHeaderBytes',
  'maxHeadersCount',
  'maxBodyBytes',
  'maxQueryBytes',
  'maxResultBytes',
  'maxHits',
  'requestTimeoutMs',
  'headersTimeoutMs',
  'headerCheckIntervalMs',
  'idleTimeoutMs',
  'keepAliveTimeoutMs',
  'maxRequestsPerSocket',
];
export function resolveWebsiteConfig(input: unknown): WebsiteConfig {
  if (
    !object(input) ||
    Object.keys(input).some((key) => key !== 'http' && key !== 'client') ||
    !object(input.http)
  )
    throw new Error('website configuration requires complete http and client policies');
  const policy = input.http;
  if (
    Object.keys(policy).length !== required.length ||
    required.some((key) => !Object.hasOwn(policy, key))
  )
    throw new Error('website http policy has missing or unknown fields');
  for (const key of required) {
    if (key === 'host') {
      if (typeof policy.host !== 'string' || isIP(policy.host) === 0)
        throw new Error('http.host must be an IP address');
    } else {
      const min = ['port', 'searchPendingRequests', 'controlPendingRequests'].includes(key) ? 0 : 1;
      // Node header-pair accounting uses a signed shift; reserve one pair to
      // detect and reject an overflow instead of silently truncating headers.
      const max =
        key === 'port'
          ? 65535
          : key === 'maxHeadersCount'
            ? 2 ** 30 - 2
            : ['maxBodyBytes', 'maxResultBytes'].includes(key)
              ? bufferLimits.MAX_LENGTH
              : 2 ** 31 - 1;
      if (!integer(policy[key], min, max)) throw new Error(`invalid http.${key}`);
    }
  }
  const http = policy as unknown as HttpPolicy;
  if (
    http.headersTimeoutMs > http.requestTimeoutMs ||
    http.headerCheckIntervalMs > http.headersTimeoutMs ||
    http.maxQueryBytes > http.maxBodyBytes
  )
    throw new Error('website timeout/query budgets are inconsistent');
  // Public SDK construction validates/copies its complete client owner policy
  // without I/O; retain that one instance rather than resolving a second policy.
  const client = new DocsbaseClient(input.client as ClientOptions);
  return Object.freeze({ http: Object.freeze({ ...http }), client });
}
