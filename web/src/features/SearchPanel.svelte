<script lang="ts">
import { onDestroy } from 'svelte';
import { api, search } from '../shared/api/client';
import type { Hit } from '../shared/api/client';
import type { ProjectView, ConfigView, SessionView, SearchRequest } from '../shared/api/generated';
import { Latest } from '../shared/api/latest.js';
let { current, settings, info }: { current: ProjectView | undefined; settings: ConfigView | null; info: SessionView | null } = $props();
let selected = $derived(current?.id ?? null);
const searches = new Latest();
function message(e: unknown) { return e instanceof Error ? e.message : 'Operation failed'; }
function cancel() { searches.cancel(); if (searchState === 'loading') searchState = 'cancelled'; }
  let query = $state('');
  let mode = $state('syntax');
  let limit = $state<number | undefined>();
  let prefix = $state('');
  let tags = $state('');
  let dedupe = $state(false);
  let rerank = $state('default');
  let hits = $state<Hit[]>([]);
  let meta = $state<unknown>(null);
  let searchState = $state('idle');
  let searchError = $state('');
  async function submit(event: SubmitEvent) {
    event.preventDefault(); if (selected === null || !query.trim()) return;
    const request = searches.start(); const id = selected;
    searchState = 'loading'; searchError = ''; hits = []; meta = null;
    const body: SearchRequest = { query, query_mode: mode, limit: limit ?? null, snippet: true, explain: true, path_prefix: prefix || null, tags: tags.trim() ? tags.split(',').map(t => t.trim()).filter(Boolean) : null, dedupe: dedupe ? 'path' : null, n_per_doc: null, rerank: rerank === 'default' ? null : rerank === 'on' };
    try {
      const value = search(await api(`projects/${id}/search`, request.signal, Object.fromEntries(Object.entries(body).filter(([,v]) => v !== null))));
      if (!searches.current(request.generation) || selected !== id) return;
      hits = value.hits; meta = value.meta; searchState = 'done';
    } catch(e) { if (searches.current(request.generation)) { searchState = 'error'; searchError = message(e); } }
  }
onDestroy(() => searches.cancel());
</script>

        <form class="panel search-form" onsubmit={submit}><label for="query">Search {current?.name ?? 'documentation'}</label><div class="search-row"><input id="query" bind:value={query} placeholder="Ask about a concept, identifier, or decision…" required autocomplete="off"/><button class="primary" disabled={!selected || !query.trim()}>Search</button>{#if searchState === 'loading'}<button type="button" class="secondary" onclick={cancel}>Cancel</button>{/if}</div><div class="filters"><label>Interpretation<select bind:value={mode}><option value="syntax">Syntax</option><option value="literal" disabled={!info?.daemon.capabilities.includes('literal-query-v1')}>Literal</option></select></label><label>Results<input type="number" bind:value={limit} min="1" max={info?.daemon.max_hits} placeholder="Daemon default"/></label><label>Path prefix<input bind:value={prefix} placeholder="docs/"/></label><label>Tags<input bind:value={tags} placeholder="api, architecture"/></label><label>Reranker<select bind:value={rerank}><option value="default">Project default</option><option value="off">Off</option><option value="on" disabled={!settings?.reranker}>On</option></select></label></div><label class="checkbox"><input type="checkbox" bind:checked={dedupe}/> Limit repeated documents</label><p class="help">Mode: <b>{settings?.search_mode ?? 'unavailable'}</b>. Search runs only when submitted. Configured remote providers may incur costs.</p>{#if !info?.daemon.capabilities.includes('literal-query-v1')}<p class="help">Literal is unavailable in this daemon.</p>{/if}</form>
        <div aria-live="polite">{#if searchState === 'loading'}<div class="empty">Searching documentation…</div>{:else if searchState === 'error'}<div class="notice error" role="alert">{searchError}</div>{:else if searchState === 'cancelled'}<div class="notice">Search cancelled. No automatic retry.</div>{:else if searchState === 'done' && !hits.length}<div class="empty"><h2>No matches</h2><p>Try another phrase or relax your filters.</p></div>{:else if searchState === 'idle'}<div class="empty"><span class="empty-icon" aria-hidden="true">⌕</span><h2>Your next answer starts here</h2><p>Search your project’s indexed documentation.</p></div>{/if}</div>
        {#if hits.length}<div class="section-label">{hits.length} RESULTS</div>{#each hits as hit}<article class="result"><div class="result-heading"><h2>{Array.isArray(hit.heading_path) ? hit.heading_path.join(' › ') || hit.path : hit.heading_path || hit.path}</h2><span class="score">score {hit.score.toFixed(4)}</span></div><code class="wrap">{hit.path} · lines {JSON.stringify(hit.lines)}</code>{#if hit.snippet}<pre>{hit.snippet}</pre>{/if}</article>{/each}{/if}
        {#if meta}<details class="panel"><summary>Search diagnostics</summary><pre>{JSON.stringify(meta, null, 2)}</pre></details>{/if}
