# Pragmatic Svelte 5 Guidelines

These rules apply to Svelte, SvelteKit, and frontend TypeScript work under `ui/`. They are adapted from [Svelte 5 Best Practices by ejirocodes](https://github.com/ejirocodes/agent-skills/tree/main/svelte/skills/svelte5-best-practices) (MIT) and tailored to KeepPeek's Svelte 5 application.

## Modern Svelte Is Required (M-SVELTE5-MODERN)

Write new components in Svelte 5 runes mode and improve touched legacy code when doing so is behavior-preserving and local.

- Use `$state`, `$derived`, `$effect`, `$props`, `$bindable`, and `$inspect` according to their specific roles.
- Use snippets and `{@render}` for component composition.
- Use event properties such as `onclick` and typed callback props.
- Use `$app/state` rather than deprecated SvelteKit stores in new code.
- Do not introduce `export let`, `$:`, `on:event`, `createEventDispatcher`, `<slot>`, `$$props`, or `$$restProps`.
- Do not rewrite generated or vendored UI primitives solely for stylistic migration. Follow their established Svelte 5 patterns when extending them.

## Model Mutable State Explicitly (M-STATE)

Use `$state` only for values that change and affect rendering or reactive work.

```svelte
<script lang="ts">
	let loading = $state(true);
	let selectedCameraId = $state('');
</script>
```

- Plain `let` and `const` remain appropriate for non-reactive local values.
- Prefer immutable replacement for API response objects and collections.
- Consider `$state.raw` for large values that are replaced as a whole and do not need deep proxying.
- Keep state as close as possible to its owner. Do not create module-level mutable state that can leak across SSR requests.
- Put reusable reactive state in `.svelte.ts` modules using runes, classes, or factory functions. Do not hide shared mutable state in ordinary TypeScript modules.

## Derive Instead of Synchronizing (M-DERIVED)

Use `$derived` for pure values computed from reactive inputs. Use `$derived.by` when the calculation needs multiple statements.

```svelte
<script lang="ts">
	let segments = $state<RecordingSegment[]>([]);
	let orderedSegments = $derived(segments.toSorted((left, right) => left.start_ms - right.start_ms));
</script>
```

- A derived expression must not perform I/O, mutate state, or alter the DOM.
- Do not use `$effect` to keep one state variable synchronized with another when the second value can be derived.
- Do not eagerly cache trivial values or add manual memoization around Svelte's dependency tracking.

## Effects Synchronize External Systems (M-EFFECTS)

Use `$effect` only to synchronize reactive state with browser APIs, media elements, subscriptions, timers, imperative libraries, or other external systems.

```svelte
$effect(() => {
	const controller = new AbortController();
	void loadCamera(cameraId, controller.signal);

	return () => controller.abort();
});
```

- Return cleanup for listeners, timers, subscriptions, observers, peer connections, and abortable work.
- Guard asynchronous completions against stale state or cancellation.
- Do not use `$effect` as a general initialization hook or as a replacement for `$derived`.
- Use `onMount` when work is strictly a one-time browser lifecycle operation. Effects do not run during SSR.
- Use `$effect.pre` only when work must happen before the DOM update, such as preserving scroll position.
- Use `untrack` only when intentionally excluding a dependency, and make that intent obvious in the surrounding code.

## Type Component Contracts (M-PROPS)

Destructure typed component props with `$props`. Make defaults and optionality visible in the type.

```svelte
<script lang="ts">
	import type { Snippet } from 'svelte';

	type Props = {
		cameraId: string;
		children?: Snippet;
		onselect?: (cameraId: string) => void;
	};

	let { cameraId, children, onselect }: Props = $props();
</script>
```

- Prefer a named `Props` type when a component contract is more than a few fields.
- Use `ComponentProps`, DOM attribute types, and existing local helper types when wrapping components or elements.
- Use `<script lang="ts" generics="T">` for genuinely generic components rather than weakening types with `any`.
- Forward only the props and element attributes the abstraction owns. Avoid opaque prop bags unless implementing a primitive wrapper.
- Do not mutate incoming props unless the contract explicitly uses binding.

## Binding Must Be Intentional (M-BINDABLE)

Props are one-way by default. Use `$bindable` only when two-way ownership is part of the component API.

```svelte
<script lang="ts">
	type Props = { value?: string };
	let { value = $bindable('') }: Props = $props();
</script>
```

- Prefer callback props for commands and domain events.
- Avoid binding merely to save a callback; it obscures ownership and makes state flow harder to reason about.
- Document a public bindable prop when its ownership semantics are not obvious from the component name.

## Use Snippets for Composition (M-SNIPPETS)

Use typed snippets instead of slots for parent-provided content.

```svelte
<script lang="ts">
	import type { Snippet } from 'svelte';
	let { header, children }: { header?: Snippet; children: Snippet } = $props();
</script>

{@render header?.()}
{@render children()}
```

- Use snippet parameters for scoped content rather than slot props.
- Keep snippets near their render site unless reuse improves clarity.
- Do not introduce `<slot>`, named slots, or `let:` slot props in new code.

## Events Are Properties and Callbacks (M-EVENTS)

Use DOM event properties and callback props.

```svelte
<button type="button" onclick={() => onselect?.(cameraId)}>Select</button>
```

- Replace event modifiers with explicit handler logic where needed, for example `event.preventDefault()`.
- Type callback payloads and DOM events precisely.
- Name domain callbacks for their action, such as `onselect`, `onseek`, or `onclose`.
- Do not use `createEventDispatcher` in new components.
- Avoid inline handlers when the logic is substantial, reused, or needs a meaningful name.

## Keep SvelteKit Boundaries Clear (M-SVELTEKIT)

- Use generated `./$types` types for `load` functions and form actions.
- Put secrets, privileged operations, and server-only dependencies in `+page.server.ts`, `+layout.server.ts`, or server modules.
- Keep universal `load` functions safe to execute in both browser and server environments.
- Run independent requests concurrently with `Promise.all`; do not serialize unrelated network work.
- Use SvelteKit `error`, `redirect`, and form actions instead of ad hoc response conventions.
- Use `use:enhance` when a form should retain progressive enhancement.
- Declare invalidation dependencies intentionally with `depends` when cached data must be refreshed.
- Read browser globals only in browser-safe code, `onMount`, or behind `browser` from `$app/environment`.

## Preserve SSR Isolation (M-SSR-ISOLATION)

- Never store request-specific, user-specific, credential, camera-selection, or playback state in mutable module scope on the server.
- Pass request state through `load`, `locals`, context, props, or per-request factories.
- Keep browser-only state out of SSR initialization unless a deterministic server fallback exists.
- Avoid hydration-dependent markup differences. Server and initial client rendering must agree.

## Design Reactive Work for Performance (M-PERFORMANCE)

- Keep reactive dependencies narrow and calculations pure.
- Use stable keys for lists whose items can be inserted, removed, or reordered.
- Do not recreate WebRTC sessions, media sources, timers, or fetches because an unrelated dependency changed.
- Cancel stale fetches and teardown media resources promptly.
- Avoid deep reactive proxies for large immutable API payloads.
- Parallelize independent SvelteKit and API work.
- Preserve layout dimensions for video, timelines, controls, and loading states so reactive updates do not shift the interface.
- Optimize measured hot paths and high-frequency media/timeline work; do not add speculative abstractions.

## Keep Components Focused (M-COMPONENTS)

- Extract a component when it owns a coherent interaction, reusable visual primitive, or independently testable state boundary.
- Do not fragment straightforward markup into pass-through components.
- Keep API access in `ui/src/lib/api.ts` and shared wire types in `ui/src/lib/types.ts` unless a feature has a justified local boundary.
- Reuse existing KeepPeek primitives in `ui/src/lib/components/ui` and preserve their bits-ui composition patterns.
- Use Lucide Svelte icons already available in the repository instead of custom inline SVGs.

## Accessibility Is Functional Correctness (M-ACCESSIBILITY)

- Prefer semantic HTML and native controls.
- Associate labels with form controls and expose accessible names for icon-only buttons.
- Preserve keyboard operation and visible focus states.
- Do not attach click behavior to non-interactive elements when a button or link is correct.
- Treat `svelte-check` accessibility diagnostics as defects, not cosmetic warnings.

## Validate the Touched UI (M-VALIDATE)

After changing anything under `ui/`, run the narrowest relevant checks and finish from the repository root with the platform script:

```sh
./check.sh
```

```bat
.\check.bat
```

- The full platform script must pass before the work is considered complete. CI uses the same entry points.
- Add focused tests when the touched behavior has an existing test surface or meaningful regression risk.
- Verify user-facing workflows in a browser for playback, WebRTC, timeline, responsive, or interaction changes.
- Do not suppress Svelte or TypeScript diagnostics to make validation pass.
- Use Bun as the only package manager and script runner under `ui/`. Run Vitest through `bun run test:unit`; do not use Bun's separate built-in test runner for this project.
- Resolve JavaScript packages only through the public npm registry configured by `.npmrc` and `ui/bunfig.toml`. Do not add private mirrors or alternate lockfiles.

## Review Checklist

- New reactive code uses the correct rune.
- Computed values use `$derived`, not synchronization effects.
- Effects have a real external system and clean up resources.
- Props, snippets, callbacks, and bindings are explicitly typed.
- No legacy Svelte 4 APIs were introduced.
- SSR state is request-isolated and hydration-safe.
- Independent async work runs concurrently.
- Media and high-frequency UI resources are stable and disposed correctly.
- Accessibility and repository UI conventions are preserved.
- `check`, `build`, and behavior-scoped validation pass.
