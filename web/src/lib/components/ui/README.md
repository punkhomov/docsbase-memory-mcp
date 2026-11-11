# UI primitives

Copied from [shadcn-svelte](https://github.com/huntabyte/shadcn-svelte/tree/6149e14107a9de3530a3083de2f1b7623b3c5d1f/docs/src/lib/registry/ui)
at commit `6149e14107a9de3530a3083de2f1b7623b3c5d1f` (2026-10-23).
The upstream MIT notice is preserved in [LICENSE.md](LICENSE.md).

Local adaptations: the native select uses Lucide's ChevronDown directly;
`src/lib/utils.ts` implements `cn` using clsx and tailwind-merge; the `cn-*`
classes use docsbase's existing sage theme in `src/style.css`. Tailwind utilities
are included without a global reset so the established layout is preserved.
Button, Input, Card, Badge, and NativeSelect are used by the application.
Native select behavior, labels, keyboard controls and explicit form submission
remain intact. No downloaded CLI is run to update these files.

Lucide is imported from the pinned `@lucide/svelte` package; its license is
included by that dependency. Icons are decorative alongside visible labels.
