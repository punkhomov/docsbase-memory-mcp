<script lang="ts">
import { onMount } from 'svelte';
import {
  ArrowRight,
  CircleCheck,
  CircleAlert,
  FolderClosed,
  Search,
  Settings2,
  RefreshCw,
  Copy,
  LoaderCircle,
} from '@lucide/svelte';
import { Button } from '$lib/components/ui/button';
import { Badge } from '$lib/components/ui/badge';
import * as Card from '$lib/components/ui/card';
import * as NativeSelect from '$lib/components/ui/native-select';
import SearchPanel from './features/SearchPanel.svelte';
import { api, session, projects, config, project as parseProject } from './shared/api/client';
import type { ProjectView, ConfigView, SessionView } from './shared/api/generated';
import { Latest } from './shared/api/latest.js';
let bootstrapToken = new URLSearchParams(location.hash.slice(1)).get('bootstrap');
history.replaceState(null, '', location.pathname);
let info = $state<SessionView | null>(null);
let rows = $state<ProjectView[]>([]);
let selected = $state<number | null>(null);
let current = $derived(rows.find((p) => p.id === selected));
let settings = $state<ConfigView | null>(null);
let page = $state('Projects');
let connection = $state('Connecting');
let error = $state('');
let loading = $state(true);
let contextLoading = $state(false);
let copied = $state(false);
const context = new Latest();
let refreshRunning = false;
let stopped = false;
let timer: ReturnType<typeof setTimeout>;
const polling = new AbortController();
let pollContext: AbortController | null = null;
function message(e: unknown) {
  return e instanceof Error ? e.message : 'Operation failed';
}
async function choose(id: number) {
  pollContext?.abort();
  selected = id;
  settings = null;
  error = '';
  contextLoading = true;
  const request = context.start();
  try {
    const [c, p] = await Promise.allSettled([
      api(`projects/${id}/config`, request.signal),
      api(`projects/${id}/status`, request.signal),
    ]).then((results) =>
      results.map((result) => {
        if (result.status === 'rejected') throw result.reason;
        return result.value;
      }),
    );
    if (!context.current(request.generation)) return;
    settings = config(c);
    const updated = parseProject(p);
    rows = rows.map((row) => (row.id === id ? updated : row));
  } catch (e) {
    if (context.current(request.generation)) error = message(e);
  } finally {
    if (context.current(request.generation)) contextLoading = false;
  }
}
async function refresh() {
  if (refreshRunning || stopped || document.hidden) return;
  refreshRunning = true;
  clearTimeout(timer);
  try {
    if (!info) {
      await initialize();
      return;
    }
    const [s, list] = await Promise.allSettled([
      api('session', polling.signal),
      api('projects', polling.signal),
    ]).then((results) =>
      results.map((result) => {
        if (result.status === 'rejected') throw result.reason;
        return result.value;
      }),
    );
    if (stopped) return;
    info = session(s);
    rows = projects(list);
    connection = 'Connected';
    error = '';
    if (selected !== null && !rows.some((p) => p.id === selected)) {
      context.cancel();
      pollContext?.abort();
      selected = null;
      settings = null;
    }
    const projectId = selected;
    const generation = context.generation;
    if (projectId !== null && !contextLoading) {
      const request = new AbortController();
      pollContext = request;
      try {
        const effective = config(await api(`projects/${projectId}/config`, request.signal));
        if (!stopped && selected === projectId && context.current(generation)) settings = effective;
      } catch (e) {
        if (
          !stopped &&
          !request.signal.aborted &&
          selected === projectId &&
          context.current(generation)
        ) {
          connection = 'Disconnected · last known data';
          error = message(e);
        }
      } finally {
        if (pollContext === request) pollContext = null;
      }
    }
  } catch (e) {
    if (!stopped) {
      connection = 'Disconnected · last known data';
      error = message(e);
    }
  } finally {
    refreshRunning = false;
    if (!stopped && info) timer = setTimeout(refresh, info.poll_interval_ms);
  }
}
async function initialize() {
  try {
    if (bootstrapToken) {
      try {
        await api('session', polling.signal, { token: bootstrapToken });
      } catch (e) {
        await api('session', polling.signal);
      } // The exchange may have succeeded before its response was lost.
      bootstrapToken = null;
    }
    const next = session(await api('session', polling.signal));
    const list = projects(await api('projects', polling.signal));
    if (stopped) return;
    info = next;
    rows = list;
    connection = 'Connected';
    error = '';
    const initial = info.initial_project ?? rows[0]?.id;
    if (initial !== undefined && initial !== null) await choose(initial);
  } catch (e) {
    if (!stopped) {
      error = message(e);
      connection = 'Disconnected';
    }
  } finally {
    loading = false;
  }
}
async function copyConfig() {
  try {
    await navigator.clipboard.writeText(JSON.stringify(settings, null, 2));
    copied = true;
  } catch {
    error = 'Copy unavailable in this browser';
  }
}
onMount(() => {
  refresh();
  const visible = () => {
    if (!document.hidden) refresh();
  };
  document.addEventListener('visibilitychange', visible);
  return () => {
    stopped = true;
    clearTimeout(timer);
    context.cancel();
    pollContext?.abort();
    polling.abort();
    document.removeEventListener('visibilitychange', visible);
  };
});
</script>
<div class="shell">
  <aside>
    <a class="brand" href="/" onclick={(e) => e.preventDefault()}
      ><span class="mark">d</span>docsbase<Badge class="badge" variant="secondary">LOCAL</Badge></a
    >
    <p class="eyebrow">WORKSPACE</p>
    <nav aria-label="Main navigation">
      {#each ['Projects', 'Search', 'Configuration'] as item}
        <Button
          variant="ghost"
          class={page === item ? 'active' : ''}
          onclick={() => {
            page = item;
          }}
          aria-current={page === item ? 'page' : undefined}
          ><span class="nav-icon" aria-hidden="true"
            >{#if item === 'Projects'}
              <FolderClosed size={18} />
            {:else if item === 'Search'}
              <Search size={18} />
            {:else}
              <Settings2 size={18} />
            {/if}</span
          >{item}</Button
        >
      {/each}
    </nav>
    <div class="aside-note">
      Your documentation.<br>Within reach.<small>Local interface · v{info?.version ?? '—'}</small>
    </div>
  </aside>
  <div class="workspace">
    <header>
      <div class="project-picker">
        <label for="project">PROJECT</label
        ><NativeSelect.Root
          id="project"
          value={selected ?? ''}
          onchange={(e) => choose(Number(e.currentTarget.value))}
          disabled={!rows.length}
          ><NativeSelect.Option value="" disabled>Select a project</NativeSelect.Option>
          {#each rows as row}
            <NativeSelect.Option value={row.id}>{row.name}</NativeSelect.Option>
          {/each}</NativeSelect.Root
        >
      </div>
      <span class:offline={connection !== 'Connected'} class="connection"
        ><span class="connection-icon" aria-hidden="true"
          >{#if connection === 'Connected'}
            <CircleCheck size={13} />
          {:else if connection === 'Connecting'}
            <LoaderCircle class="spin" size={13} />
          {:else}
            <CircleAlert size={13} />
          {/if}</span
        > {connection}</span
      >
    </header>
    <main>
      <div class="page-heading">
        <div>
          <p class="eyebrow">DOCUMENTATION MEMORY</p>
          <h1>{page}</h1>
          <p class="subheading">
            {page === 'Projects'
              ? 'A clear view of your indexed knowledge.'
              : page === 'Search'
                ? 'Find the details that move your work forward.'
                : 'The effective settings used by your daemon.'}
          </p>
        </div>
        {#if page === 'Projects'}
          <Button variant="outline" class="secondary" onclick={refresh}
            ><RefreshCw size={14} aria-hidden="true" />Refresh</Button
          >
        {/if}
      </div>
      {#if error}
        <div class="notice error" role="alert">{error}</div>
      {/if}
      {#if loading}
        <div class="empty" role="status">Connecting to your local daemon…</div>
      {:else if !info}
        <div class="empty">
          <h2>Connection unavailable</h2>
          <p>Check the daemon and retry. No search will be replayed.</p>
          <Button variant="outline" onclick={refresh}>Retry connection</Button>
        </div>
      {:else if !rows.length}
        <div class="empty">
          <h2>No projects yet</h2>
          <p>Run <code>docsbase index</code> in a documentation folder, then refresh.</p>
          <Button variant="outline" onclick={refresh}>Refresh projects</Button>
        </div>
      {:else if page === 'Projects'}
        <div class="section-label">REGISTERED PROJECTS <span>{rows.length}</span></div>
        <div class="project-grid">
          {#each rows as row}
            <button
              type="button"
              class="project-card"
              class:selected={selected === row.id}
              onclick={() => choose(row.id)}
            >
              <Card.Root class="project-card-body"
                ><div class="card-top">
                  <FolderClosed class="folder" size={24} aria-hidden="true" />
                  <Badge variant="secondary" class="status">{row.status}</Badge>
                </div>
                <h2>{row.name}</h2>
                <code>{row.root}</code>
                <div class="counts">
                  <span><b>{row.docs}</b> documents</span><span><b>{row.chunks}</b> chunks</span>
                </div></Card.Root
              >
            </button>
          {/each}
        </div>
        {#if current}
          <Card.Root class="panel"
            ><div class="section-label">SELECTED PROJECT</div>
            <h2>{current.name}</h2>
            <p class="mono wrap">{current.root}</p>
            <div class="facts">
              <div>
                <small>Search mode</small><strong>{settings?.search_mode ?? 'Unavailable'}</strong>
              </div>
              <div>
                <small>Interpretation</small
                ><strong
                  >{info?.daemon.capabilities.includes('literal-query-v1')
                    ? 'Syntax + Literal'
                    : 'Syntax'}</strong
                >
              </div>
              <div>
                <small>Reranker configured</small
                ><strong>{settings ? (settings.reranker ? 'Yes' : 'No') : 'Unavailable'}</strong>
              </div>
              <div>
                <small>Watcher</small><strong>{current.watched ? 'Active' : 'Inactive'}</strong>
              </div>
              <div>
                <small>Embedding coverage</small
                ><strong
                  >{current.cached_vectors === null
                    ? 'Unavailable'
                    : `${current.cached_vectors} / ${current.vector_chunks}`}</strong
                >
              </div>
              <div>
                <small>ANN state</small><strong>{current.ann_state ?? 'Unavailable'}</strong>
              </div>
            </div>
            <Button class="primary" onclick={() => (page = 'Search')}
              >Search this project <ArrowRight size={16} aria-hidden="true" /></Button
            ></Card.Root
          >
        {/if}
      {:else if page === 'Search'}
        {#key selected}
          <SearchPanel {current} {settings} {info} />
        {/key}
      {:else if page === 'Configuration'}
        {#if contextLoading}
          <div class="empty">Loading effective configuration…</div>
        {:else if settings}
          <div class="config-toolbar">
            <p class="help">
              Read-only · secrets are excluded before transmission. Unavailable sources are not
              inferred.
            </p>
            <Button variant="outline" class="secondary" onclick={copyConfig}
              ><Copy size={14} aria-hidden="true" />
              {copied ? 'Copied' : 'Copy safe configuration'}</Button
            >
          </div>
          {#each [...new Set(settings.fields.map((f) => f.group))] as group}
            <Card.Root class="panel"
              ><h2>{group}</h2>
              <dl class="config-list">
                {#each settings.fields.filter((f) => f.group === group) as field}
                  <div>
                    <dt>{field.name}<small>Source: {field.source ?? 'Unavailable'}</small></dt>
                    <dd>{field.value || '—'}</dd>
                  </div>
                {/each}
              </dl></Card.Root
            >
          {/each}
        {:else}
          <div class="empty">
            Configuration unavailable.<Button
              variant="outline"
              onclick={() => selected !== null && choose(selected)}
              >Retry</Button
            >
          </div>
        {/if}
      {/if}
    </main>
    <footer>
      docsbase <a href="/third-party-notices.txt">Licenses</a
      ><span>Files remain the source of truth.</span>
    </footer>
  </div>
</div>
