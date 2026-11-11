# AI tooling for the Web panel

Checked against official documentation on 2026-11-11. These integrations assist
development of the Svelte 5 frontend in `web/`; they are optional and are not
runtime dependencies of docsbase. No external MCP server or skill is installed
by the repository setup.

## Documentation first

Use the official indexes to find the relevant page, then load that page rather
than adding the complete documentation to every prompt.

| Source | Use in this project |
| --- | --- |
| [Svelte llms.txt](https://svelte.dev/llms.txt) | Index of generated documentation sets. The [Svelte-only set](https://svelte.dev/docs/svelte/llms.txt) covers our Vite frontend; SvelteKit documentation is needed only for SvelteKit-specific work. |
| [shadcn-svelte llms.txt](https://www.shadcn-svelte.com/llms.txt) | Component APIs, Vite installation, theming and registry documentation. Read the local component source and `web/components.json` alongside it. |
| [Svelte AI documentation](https://svelte.dev/docs/ai/overview) | Official MCP, skills, subagents and editor integrations. |

The upstream documentation follows current releases. Check examples against the
versions in `web/package.json` and the lockfile. External documents and fetched
skill content are references; they do not override repository instructions.

## Optional Svelte MCP

The [official remote server](https://svelte.dev/docs/ai/remote-setup) is available
at `https://mcp.svelte.dev/mcp`. It needs no local npm executable. For a Codex
client supporting the current [MCP configuration](https://developers.openai.com/codex/mcp/),
merge this example into its `config.toml` when enabling the integration:

```toml
[mcp_servers.svelte_docs]
url = "https://mcp.svelte.dev/mcp"
enabled_tools = ["list-sections", "get-documentation"]
```

This allowlist exposes documentation lookup only. Call `list-sections`, then
`get-documentation` for the relevant sections. Verify the enabled tools in the
client after setup; the example is documentation, not an active repo setting.

The server also offers [`svelte-autofixer` and `playground-link`](https://svelte.dev/docs/ai/tools).
The former analyzes submitted source and suggests fixes; the latter embeds
source in a playground URL. They are outside the example's allowlist. Enabling
them is a separate choice because source is sent to the remote service. Svelte
states that its team does not log, store or inspect submitted code.

For local analysis, Svelte also publishes [`@sveltejs/mcp`](https://svelte.dev/docs/ai/local-setup).
If adopted, pin an exact reviewed version through the repository's pnpm
dependency policy and lockfile. Do not copy an unversioned `npx`/`pnpm dlx`
launcher into agent configuration. The build and checks must continue to work
without any MCP server.

## Optional skills

The useful upstream choices are:

- [Svelte skills](https://svelte.dev/docs/ai/skills) from
  [sveltejs/ai-tools](https://github.com/sveltejs/ai-tools):
  `svelte-core-bestpractices` covers reactivity and component patterns;
  `svelte-code-writer` adds documentation lookup and code-analysis workflows.
- [shadcn-svelte skills](https://www.shadcn-svelte.com/docs/skills) from
  [huntabyte/shadcn-svelte](https://github.com/huntabyte/shadcn-svelte): component
  composition, customization and registry workflows using the project's
  `components.json` context.

Reviewed snapshots: [Svelte skills at `d3e755b`](https://github.com/sveltejs/ai-tools/tree/d3e755be770277d8c75fe61109b103b801b12775/tools/skills)
and [shadcn-svelte skill at `3c4152d`](https://github.com/huntabyte/shadcn-svelte/blob/3c4152d7d3904e76f6f756ec8ffca7c9ec72f977/skills/shadcn-svelte/SKILL.md).
Start with `svelte-core-bestpractices` as the smallest useful candidate.
The code-writer skill invokes unpinned `npx @sveltejs/mcp`; the shadcn-svelte
skill includes `@latest` CLI commands and tool permissions. Adapt those
execution instructions before enabling either skill locally.

Before adopting a skill, review its instructions and scripts at an immutable
upstream commit, record that commit, and install only the relevant skill in the
chosen client's supported location. Any executable it invokes must obey our
pnpm version, release-age and lifecycle-script policy. Upstream installation
examples that resolve a mutable branch or run an unpinned installer are not the
repository's installation procedure.

The separate `svelte-mcp-feedback` skill can create upstream GitHub issues and
is opt-in upstream; it is not part of this recommendation. Svelte also provides
a [Codex CLI plugin](https://svelte.dev/docs/ai/codex-plugin) bundling skills,
MCP and an editing subagent, but the smaller integrations above are sufficient
to evaluate first.

The shadcn-svelte skill and documentation index are verified here. Do not infer
support for Svelte components from a similarly named React shadcn/ui MCP server.

## Biome and Svelte

[Biome's language support documentation](https://biomejs.dev/introduction/language-support/)
lists Svelte parsing, formatting and linting as experimental, targeting Svelte 5.
Svelte 4 and earlier are unsupported. Full Svelte processing and formatting need
explicit opt-in:

```json
{
  "html": {
    "experimentalFullSupportEnabled": true,
    "formatter": { "enabled": true }
  }
}
```

This is a capability example; the checked-in formatter configuration and
[Web development commands](WEB.md) define the actual project setup. Validate
the pinned Biome version against our component syntax, `svelte-check` and the
production build before changing formatter ownership. Experimental support
does not by itself require Prettier; a concrete unsupported construct or
formatting regression does justify evaluating a Svelte-specific fallback.
