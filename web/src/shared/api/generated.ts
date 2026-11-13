// Generated from Rust contracts. Do not edit.
export type SearchHit = { chunk_id: number, path: string, heading_path: string, lines: [number, number], score: number, snippet?: string, snippet_truncated?: boolean, };
export type SearchResponse = Array<SearchHit> | { hits: Array<SearchHit>, meta: unknown, };
export type ProjectView = { id: number, name: string, root: string, status: string, docs: number, chunks: number, watched: boolean, cached_vectors: number | null, vector_chunks: number | null, ann_state: string | null, };
export type ConfigField = { group: string, name: string, value: string, source: string | null, };
export type ConfigView = { fields: Array<ConfigField>, search_mode: string, reranker: boolean, };
export type PanelInfo = { capabilities: Array<string>, request_timeout_ms: number, max_request_payload_bytes: number, max_retained_payload_bytes: number, dedupe_max_candidates: number, max_hits: number, };
export type SessionView = { version: string, poll_interval_ms: number, initial_project: number | null, daemon: PanelInfo, };
export type SearchRequest = { query: string, query_mode: string | null, limit: number | null, rerank: boolean | null, explain: boolean | null, snippet: boolean | null, dedupe: string | null, n_per_doc: number | null, path_prefix: string | null, tags: Array<string> | null, };
export type ApiError = { code: string, message: string, request_id: string, };
