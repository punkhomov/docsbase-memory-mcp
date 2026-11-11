import type { SessionView, ProjectView, ConfigView, SearchHit } from './generated';
export async function api(path: string, signal?: AbortSignal, body?: unknown): Promise<unknown> {
  const response = await fetch(`/api/v1/${path}`, { signal, credentials: 'same-origin', cache: 'no-store', method: body === undefined ? 'GET' : 'POST', headers: body === undefined ? {} : { 'Content-Type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
  const data = await response.json();
  if (!response.ok) throw new Error(`${typeof data.message === 'string' ? data.message : 'Request failed'} (${data.code ?? response.status}; request ${data.request_id ?? 'unavailable'})`);
  return data;
}
export function record(value: unknown): Record<string, any> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Incompatible server response');
  return value as Record<string, any>;
}
export function project(value: unknown): ProjectView {
  const v = record(value);
  if (!Number.isSafeInteger(v.id) || !['name','root','status'].every(k => typeof v[k] === 'string') || !['docs','chunks'].every(k => Number.isSafeInteger(v[k]) && v[k] >= 0)) throw new Error('Incompatible project response');
  if (typeof v.watched !== 'boolean' || !['cached_vectors','vector_chunks'].every(k => v[k] === null || (Number.isInteger(v[k]) && v[k] >= 0)) || !(v.ann_state === null || typeof v.ann_state === 'string')) throw new Error('Incompatible project status');
  return v as ProjectView;
}
export function projects(value: unknown): ProjectView[] {
  if (!Array.isArray(value)) throw new Error('Incompatible project list');
  return value.map(project);
}
export function session(value: unknown): SessionView {
  const v = record(value); const d = record(v.daemon);
  if (typeof v.version !== 'string' || !Number.isSafeInteger(v.poll_interval_ms) || v.poll_interval_ms <= 0 || !Array.isArray(d.capabilities) || !d.capabilities.every(x => typeof x === 'string') || !Number.isSafeInteger(d.max_hits) || d.max_hits <= 0 || !(v.initial_project === null || Number.isSafeInteger(v.initial_project))) throw new Error('Incompatible session response');
  return v as SessionView;
}
export function config(value: unknown): ConfigView {
  const v = record(value);
  if (!Array.isArray(v.fields) || !v.fields.every(f => f && ['group','name','value'].every(k => typeof f[k] === 'string') && (f.source === null || typeof f.source === 'string')) || typeof v.search_mode !== 'string' || typeof v.reranker !== 'boolean') throw new Error('Incompatible configuration response');
  return v as ConfigView;
}
export type Hit = SearchHit;
export function search(value: unknown): { hits: Hit[]; meta: unknown } {
  const data = Array.isArray(value) ? { hits: value, meta: null } : record(value);
  if (!Array.isArray(data.hits) || !data.hits.every(h => h && Number.isInteger(h.chunk_id) && h.chunk_id >= 0 && typeof h.path === 'string' && typeof h.heading_path === 'string' && Array.isArray(h.lines) && h.lines.length === 2 && h.lines.every(Number.isSafeInteger) && Number.isFinite(h.score) && (h.snippet === undefined || typeof h.snippet === 'string'))) throw new Error('Incompatible search response');
  return data as { hits: Hit[]; meta: unknown };
}
