/** Complete policy resolved by the embedding application's configuration boundary. */
export interface ClientSettings {
  readonly searchConnections: number;
  readonly controlConnections: number;
  readonly queueLimit: number;
  readonly requestTimeoutMs: number;
}
export interface ClientOptions {
  readonly endpoint: string;
  readonly projectId: number;
  readonly expectedBuildId: string;
  readonly expectedSchemaVersion: number;
  readonly settings: ClientSettings;
}
export interface HelloMetadata {
  readonly protocol_version: number;
  readonly build_id: string;
  readonly schema_version: number;
  readonly capabilities: readonly string[];
}
export interface SearchOptions {
  readonly query_mode?: 'syntax' | 'literal' | null;
  readonly limit?: number | null;
  readonly explain?: boolean | null;
  readonly snippet?: boolean | null;
  readonly rerank?: boolean | null;
  readonly dedupe?: 'none' | 'path' | null;
  readonly n_per_doc?: number | null;
  readonly path_prefix?: string | null;
  readonly tags?: readonly string[] | null;
}
export interface CallOptions {
  readonly signal?: AbortSignal;
  /** Original caller times in Node performance.now()'s monotonic clock domain. */
  readonly budget?: { readonly startedAtMs: number; readonly deadlineAtMs: number };
}
export interface SearchHit {
  /** Exact Rust u64 rendered in canonical decimal notation; never a JS number. */
  readonly chunk_id: string;
  readonly path: string;
  readonly heading_path: string;
  readonly lines: readonly [number, number];
  readonly score: number;
  readonly snippet?: string;
  readonly snippet_truncated?: boolean;
}
export type SearchResult = SearchHit[] | { hits: SearchHit[]; meta: Record<string, unknown> };
export interface ProjectStatus {
  readonly id: number;
  readonly name: string;
  readonly root: string;
  readonly status: string;
  readonly docs: number;
  readonly chunks: number;
  readonly index_generation: number;
  readonly watched: boolean;
}
export interface DaemonStatus {
  readonly project: ProjectStatus;
  readonly restart_required: boolean;
  readonly notice: string | null;
}
export type ErrorKind =
  | 'validation'
  | 'protocol'
  | 'transport'
  | 'daemon'
  | 'deadline'
  | 'cancelled'
  | 'overloaded'
  | 'closed';
export class DocsbaseError extends Error {
  readonly kind: ErrorKind;
  readonly code: number | undefined;
  readonly instruction: string | undefined;
  constructor(
    kind: ErrorKind,
    message: string,
    details: { code?: number; instruction?: string; cause?: unknown } = {},
  ) {
    super(message, { cause: details.cause });
    this.name = 'DocsbaseError';
    this.kind = kind;
    this.code = details.code;
    this.instruction = details.instruction;
  }
}
export function abortError(signal: AbortSignal): DocsbaseError {
  return signal.reason instanceof DocsbaseError
    ? signal.reason
    : new DocsbaseError('cancelled', 'docsbase request cancelled');
}

/** Strict uncapped counters from the completed lexical pass. */
export interface IndexStats {
  readonly docs: number;
  readonly chunks: number;
  readonly skipped: number;
  readonly removed: number;
  readonly errors: number;
  readonly warnings: readonly { readonly path: string; readonly message: string }[];
}
export interface IndexOutcome {
  readonly schema_version: 1;
  readonly attempt_id: number;
  readonly index_generation: number;
  readonly canonical_root: string;
  readonly recipe_id: string;
  readonly scope: 'full_survey' | 'subset';
  readonly stats: IndexStats;
}
export interface ServerSync {
  readonly job_id: number;
  readonly state: 'queued' | 'running' | 'done' | 'error';
  readonly attempt_id: number | null;
  readonly stats: IndexStats | null;
}
export type ReadinessReason =
  | 'state_changed'
  | 'indexing'
  | 'interrupted'
  | 'failed'
  | 'partial'
  | 'sync_failed'
  | 'index_unavailable'
  | 'rebuild_required'
  | 'unknown_outcome'
  | 'incomplete_scope'
  | 'recipe_changed';
/** A bounded current observation; it does not pin a future search generation. */
export interface ServerStatus {
  readonly schema_version: 1;
  readonly profile: 'website';
  readonly project: {
    readonly id: number;
    readonly name: string;
    readonly root: string;
    readonly status: string;
  };
  readonly index_generation: number;
  readonly index_attempt: number;
  readonly index_health: 'healthy' | 'index_unavailable' | 'rebuild_required';
  readonly outcome: IndexOutcome | null;
  readonly active_sync: ServerSync | null;
  readonly latest_sync: ServerSync | null;
  readonly ready: boolean;
  readonly reason: ReadinessReason | null;
  readonly restart_required: boolean;
  readonly notice: string | null;
}
