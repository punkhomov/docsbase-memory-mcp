<script lang="ts">
import { onDestroy } from 'svelte';
import { Search, LoaderCircle, X, CircleAlert } from '@lucide/svelte';
import { Button } from '$lib/components/ui/button';
import { Input } from '$lib/components/ui/input';
import * as Card from '$lib/components/ui/card';
import * as NativeSelect from '$lib/components/ui/native-select';
import { api, search } from '../shared/api/client';
import type { Hit } from '../shared/api/client';
import type { ProjectView, ConfigView, SessionView, SearchRequest } from '../shared/api/generated';
import { Latest } from '../shared/api/latest.js';
let {
  current,
  settings,
  info,
}: { current: ProjectView | undefined; settings: ConfigView | null; info: SessionView | null } =
  $props();
let selected = $derived(current?.id ?? null);
const searches = new Latest();
function message(e: unknown) {
  return e instanceof Error ? e.message : 'Operation failed';
}
function cancel() {
  searches.cancel();
  if (searchState === 'loading') searchState = 'cancelled';
}
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
  event.preventDefault();
  if (selected === null || !query.trim()) return;
  const request = searches.start();
  const id = selected;
  searchState = 'loading';
  searchError = '';
  hits = [];
  meta = null;
  const body: SearchRequest = {
    query,
    query_mode: mode,
    limit: limit ?? null,
    snippet: true,
    explain: true,
    path_prefix: prefix || null,
    tags: tags.trim()
      ? tags
          .split(',')
          .map((t) => t.trim())
          .filter(Boolean)
      : null,
    dedupe: dedupe ? 'path' : null,
    n_per_doc: null,
    rerank: rerank === 'default' ? null : rerank === 'on',
  };
  try {
    const value = search(
      await api(
        `projects/${id}/search`,
        request.signal,
        Object.fromEntries(Object.entries(body).filter(([, v]) => v !== null)),
      ),
    );
    if (!searches.current(request.generation) || selected !== id) return;
    hits = value.hits;
    meta = value.meta;
    searchState = 'done';
  } catch (e) {
    if (searches.current(request.generation)) {
      searchState = 'error';
      searchError = message(e);
    }
  }
}
onDestroy(() => searches.cancel());
</script>

<form class="panel search-form" onsubmit={submit}>
  <label for="query">Search {current?.name ?? 'documentation'}</label>
  <div class="search-row">
    <Input
      id="query"
      bind:value={query}
      placeholder="Ask about a concept, identifier, or decision…"
      required
      autocomplete="off"
    /><Button type="submit" class="primary" disabled={!selected || !query.trim()}
      ><Search size={16} aria-hidden="true" />Search</Button
    >
    {#if searchState === 'loading'}
      <Button type="button" variant="outline" class="secondary" onclick={cancel}
        ><X size={16} aria-hidden="true" />Cancel</Button
      >
    {/if}
  </div>
  <div class="filters">
    <label for="interpretation"
      >Interpretation<NativeSelect.Root id="interpretation" bind:value={mode}
        ><NativeSelect.Option value="syntax">Syntax</NativeSelect.Option
        ><NativeSelect.Option
          value="literal"
          disabled={!info?.daemon.capabilities.includes('literal-query-v1')}
          >Literal</NativeSelect.Option
        ></NativeSelect.Root
      ></label
    ><label for="result-limit"
      >Results<Input
        id="result-limit"
        type="number"
        bind:value={limit}
        min="1"
        max={info?.daemon.max_hits}
        placeholder="Daemon default"
      /></label
    ><label for="path-prefix"
      >Path prefix<Input id="path-prefix" bind:value={prefix} placeholder="docs/" /></label
    ><label for="tags"
      >Tags<Input id="tags" bind:value={tags} placeholder="api, architecture" /></label
    ><label for="reranker"
      >Reranker<NativeSelect.Root id="reranker" bind:value={rerank}
        ><NativeSelect.Option value="default">Project default</NativeSelect.Option
        ><NativeSelect.Option value="off">Off</NativeSelect.Option
        ><NativeSelect.Option value="on" disabled={!settings?.reranker}
          >On</NativeSelect.Option
        ></NativeSelect.Root
      ></label
    >
  </div>
  <label class="checkbox"
    ><input type="checkbox" bind:checked={dedupe}>
    Limit repeated documents</label
  >
  <p class="help">
    Mode: <b>{settings?.search_mode ?? 'unavailable'}</b>. Search runs only when submitted.
    Configured remote providers may incur costs.
  </p>
  {#if !info?.daemon.capabilities.includes('literal-query-v1')}
    <p class="help">Literal is unavailable in this daemon.</p>
  {/if}
</form>
<div aria-live="polite">
  {#if searchState === 'loading'}
    <div class="empty" role="status">
      <LoaderCircle class="spin empty-loading" size={24} aria-hidden="true" />Searching
      documentation…
    </div>
  {:else if searchState === 'error'}
    <div class="notice error" role="alert">
      <CircleAlert size={16} aria-hidden="true" />{searchError}
    </div>
  {:else if searchState === 'cancelled'}
    <div class="notice">Search cancelled. No automatic retry.</div>
  {:else if searchState === 'done' && !hits.length}
    <div class="empty">
      <h2>No matches</h2>
      <p>Try another phrase or relax your filters.</p>
    </div>
  {:else if searchState === 'idle'}
    <div class="empty">
      <Search class="empty-icon" size={45} aria-hidden="true" />
      <h2>Your next answer starts here</h2>
      <p>Search your project’s indexed documentation.</p>
    </div>
  {/if}
</div>
{#if hits.length}
  <div class="section-label">{hits.length} RESULTS</div>
  {#each hits as hit}
    <Card.Root class="result" role="article"
      ><div class="result-heading">
        <h2>
          {Array.isArray(hit.heading_path)
            ? hit.heading_path.join(' › ') || hit.path
            : hit.heading_path || hit.path}
        </h2>
        <span class="score">score {hit.score.toFixed(4)}</span>
      </div>
      <code class="wrap">{hit.path} · lines {JSON.stringify(hit.lines)}</code>
      {#if hit.snippet}
        <pre>{hit.snippet}</pre>
      {/if}</Card.Root
    >
  {/each}
{/if}
{#if meta}
  <details class="panel">
    <summary>Search diagnostics</summary>
    <pre>{JSON.stringify(meta, null, 2)}</pre>
  </details>
{/if}
