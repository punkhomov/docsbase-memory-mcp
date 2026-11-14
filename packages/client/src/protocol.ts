import {
  DocsbaseError,
  type HelloMetadata,
  type SearchHit,
  type SearchResult,
  type DaemonStatus,
  type ProjectStatus,
  type ServerStatus,
  type IndexStats,
  type IndexOutcome,
  type ServerSync,
} from './types.ts';

// Wire identities/limit owned by Rust ipc::protocol, checked by the Rust fixture gate.
export const PROTOCOL_VERSION = 1;
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;
export const SEARCH_CONTEXT = 'search-context-v1';
export const PANEL_READ = 'panel-read-v1';
export const LITERAL_QUERY = 'literal-query-v1';
export const SERVER_STATUS = 'server-status-v1';
export type WireRequest = {
  method: 'Hello' | 'BindReadOnly' | 'Search' | 'CallTool';
  params: unknown;
};
export type WireResponse =
  | { kind: 'Hello'; data: HelloMetadata }
  | { kind: 'ToolResult'; data: { value: unknown } }
  | { kind: 'Error'; data: { code: number; message: string; instruction?: string | null } };
export function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
export function protocolError(): DocsbaseError {
  return new DocsbaseError('protocol', 'invalid docsbase response');
}
export function safeUnsigned(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}
function u64(source: string | undefined): string {
  if (
    !source ||
    !/^(0|[1-9][0-9]*)$/.test(source) ||
    source.length > 20 ||
    BigInt(source) > 18446744073709551615n
  )
    throw protocolError();
  return source;
}
export function decodeResponse(bytes: Uint8Array): WireResponse {
  let value: unknown;
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    value = JSON.parse(text, (key: string, item: unknown, context?: { source?: string }) => {
      if (key === 'chunk_id') {
        if (typeof item !== 'number') throw protocolError();
        return u64(context?.source);
      }
      if (
        typeof item === 'number' &&
        (!Number.isFinite(item) || (Number.isInteger(item) && !Number.isSafeInteger(item)))
      )
        throw protocolError();
      return item;
    });
  } catch {
    throw protocolError();
  }
  if (!object(value) || !object(value.data)) throw protocolError();
  const data = value.data;
  if (value.kind === 'Hello') {
    if (
      !safeUnsigned(data.protocol_version) ||
      data.protocol_version > 0xffffffff ||
      typeof data.build_id !== 'string' ||
      !safeUnsigned(data.schema_version) ||
      data.schema_version === 0 ||
      data.schema_version > 0xffffffff ||
      !Array.isArray(data.capabilities) ||
      !data.capabilities.every((c) => typeof c === 'string')
    )
      throw protocolError();
  } else if (value.kind === 'ToolResult') {
    if (!Object.hasOwn(data, 'value')) throw protocolError();
  } else if (value.kind === 'Error') {
    if (
      typeof data.code !== 'number' ||
      !Number.isSafeInteger(data.code) ||
      data.code < -(2 ** 31) ||
      data.code > 2 ** 31 - 1 ||
      typeof data.message !== 'string' ||
      (data.instruction !== undefined &&
        data.instruction !== null &&
        typeof data.instruction !== 'string')
    )
      throw protocolError();
  } else {
    throw protocolError();
  }
  return value as WireResponse;
}
export function encodeRequest(value: WireRequest): Buffer {
  const text = JSON.stringify(value);
  if (Buffer.byteLength(text) > MAX_FRAME_BYTES) {
    throw new DocsbaseError('validation', 'docsbase request exceeds IPC frame limit');
  }
  return Buffer.from(text + '\n');
}
export function toolValue(response: WireResponse): unknown {
  if (response.kind === 'Error') {
    throw new DocsbaseError('daemon', response.data.message, {
      code: response.data.code,
      ...(typeof response.data.instruction === 'string'
        ? { instruction: response.data.instruction }
        : {}),
    });
  }
  if (response.kind !== 'ToolResult') throw protocolError();
  return response.data.value;
}
function citation(value: unknown): value is SearchHit {
  return (
    object(value) &&
    typeof value.chunk_id === 'string' &&
    /^(0|[1-9][0-9]*)$/.test(value.chunk_id) &&
    value.chunk_id.length <= 20 &&
    BigInt(value.chunk_id) <= 18446744073709551615n &&
    typeof value.path === 'string' &&
    typeof value.heading_path === 'string' &&
    Array.isArray(value.lines) &&
    value.lines.length === 2 &&
    value.lines.every((n) => safeUnsigned(n) && n <= 0xffffffff) &&
    typeof value.score === 'number' &&
    Number.isFinite(value.score) &&
    (value.snippet === undefined || typeof value.snippet === 'string') &&
    (value.snippet_truncated === undefined || typeof value.snippet_truncated === 'boolean')
  );
}
export function searchResult(value: unknown): SearchResult {
  const hits = Array.isArray(value) ? value : object(value) ? value.hits : null;
  if (
    !Array.isArray(hits) ||
    !hits.every(citation) ||
    (!Array.isArray(value) && (!object(value) || !object(value.meta)))
  )
    throw protocolError();
  return value as SearchResult;
}
export function daemonStatus(value: unknown, projectId: number): DaemonStatus {
  if (
    !object(value) ||
    !Array.isArray(value.projects) ||
    typeof value.restart_required !== 'boolean' ||
    !(value.notice === null || typeof value.notice === 'string')
  )
    throw protocolError();
  const project: unknown = value.projects.find((p) => object(p) && p.id === projectId);
  if (!object(project))
    throw new DocsbaseError('daemon', 'bound project is not registered', { code: -32012 });
  if (
    typeof project.name !== 'string' ||
    typeof project.root !== 'string' ||
    typeof project.status !== 'string' ||
    typeof project.watched !== 'boolean' ||
    !safeUnsigned(project.docs) ||
    !safeUnsigned(project.chunks) ||
    !safeUnsigned(project.index_generation)
  )
    throw protocolError();
  return {
    project: project as unknown as ProjectStatus,
    restart_required: value.restart_required,
    notice: value.notice,
  };
}

function indexStats(value: unknown): value is IndexStats {
  return (
    object(value) &&
    ['docs', 'chunks', 'skipped', 'removed', 'errors'].every((key) => safeUnsigned(value[key])) &&
    Array.isArray(value.warnings) &&
    value.warnings.length <= 100 &&
    value.warnings.every(
      (warning) =>
        object(warning) && typeof warning.path === 'string' && typeof warning.message === 'string',
    )
  );
}
function indexOutcome(value: unknown): value is IndexOutcome {
  return (
    object(value) &&
    value.schema_version === 1 &&
    safeUnsigned(value.attempt_id) &&
    value.attempt_id > 0 &&
    safeUnsigned(value.index_generation) &&
    typeof value.canonical_root === 'string' &&
    typeof value.recipe_id === 'string' &&
    /^index-recipe-v1:[a-fA-F0-9]{64}$/.test(value.recipe_id) &&
    (value.scope === 'full_survey' || value.scope === 'subset') &&
    indexStats(value.stats)
  );
}
function serverSync(value: unknown): value is ServerSync {
  return (
    object(value) &&
    safeUnsigned(value.job_id) &&
    value.job_id > 0 &&
    ['queued', 'running', 'done', 'error'].includes(value.state as string) &&
    (value.attempt_id === null || (safeUnsigned(value.attempt_id) && value.attempt_id > 0)) &&
    (value.stats === null || indexStats(value.stats))
  );
}
const readinessReasons = new Set([
  'state_changed',
  'indexing',
  'interrupted',
  'failed',
  'partial',
  'sync_failed',
  'index_unavailable',
  'rebuild_required',
  'unknown_outcome',
  'incomplete_scope',
  'recipe_changed',
]);
export function serverStatus(value: unknown, projectId: number): ServerStatus {
  if (
    !object(value) ||
    value.schema_version !== 1 ||
    value.profile !== 'website' ||
    !object(value.project) ||
    value.project.id !== projectId ||
    typeof value.project.name !== 'string' ||
    typeof value.project.root !== 'string' ||
    typeof value.project.status !== 'string' ||
    !safeUnsigned(value.index_generation) ||
    !safeUnsigned(value.index_attempt) ||
    !['healthy', 'index_unavailable', 'rebuild_required'].includes(value.index_health as string) ||
    !(value.outcome === null || indexOutcome(value.outcome)) ||
    !(
      value.active_sync === null ||
      (serverSync(value.active_sync) && ['queued', 'running'].includes(value.active_sync.state))
    ) ||
    !(value.latest_sync === null || serverSync(value.latest_sync)) ||
    typeof value.ready !== 'boolean' ||
    !(value.reason === null || readinessReasons.has(value.reason as string)) ||
    value.ready !== (value.reason === null) ||
    typeof value.restart_required !== 'boolean' ||
    !(value.notice === null || typeof value.notice === 'string')
  )
    throw protocolError();
  return value as unknown as ServerStatus;
}
