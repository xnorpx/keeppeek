<script lang="ts">
	import type { CameraListItem } from '$lib/types';
	import {
		defaultExpressionPredicate,
		MAX_EXPRESSION_DEPTH,
		summarizeEventExpression,
		validateEventExpression,
		type EventExpressionNode,
		type EventExpressionPredicate,
		type EventTextField,
		type EventWorkflowStatePredicate
	} from '$lib/event-expression';
	import type { EventPlannerDiagnostics, EventSemanticSummary } from '$lib/control-client';
	import ArrowDownIcon from '@lucide/svelte/icons/arrow-down';
	import ArrowUpIcon from '@lucide/svelte/icons/arrow-up';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';

	type Props = {
		draft: EventExpressionNode;
		cameras: readonly Pick<CameraListItem, 'id' | 'name'>[];
		canConvertToCommon: boolean;
		semanticAvailable: boolean;
		normalizedSummary?: string | null;
		semanticSummary?: EventSemanticSummary | null;
		diagnostics?: EventPlannerDiagnostics | null;
		onchange: (next: EventExpressionNode) => void;
		onapply: () => void;
		onconvert?: () => void;
	};

	let {
		draft,
		cameras,
		canConvertToCommon,
		semanticAvailable,
		normalizedSummary = null,
		semanticSummary = null,
		diagnostics = null,
		onchange,
		onapply,
		onconvert
	}: Props = $props();

	let validationError = $derived(validateEventExpression(draft));
	let summaryText = $derived(normalizedSummary || summarizeEventExpression(draft));
	let csvDrafts = $state<Record<string, string>>({});
	let lastEmittedDraft: EventExpressionNode | null = null;

	$effect(() => {
		if (draft !== lastEmittedDraft) {
			csvDrafts = {};
		}
	});

	function emitDraft(next: EventExpressionNode): void {
		lastEmittedDraft = next;
		onchange(next);
	}

	const predicateKinds: readonly { value: EventExpressionPredicate['kind']; label: string }[] = [
		{ value: 'camera_ids', label: 'Camera' },
		{ value: 'group_ids', label: 'Group' },
		{ value: 'event_types', label: 'Event type' },
		{ value: 'origins', label: 'Origin' },
		{ value: 'zones', label: 'Zone' },
		{ value: 'missing_zone', label: 'Missing zone' },
		{ value: 'confidence', label: 'Confidence' },
		{ value: 'missing_confidence', label: 'Missing confidence' },
		{ value: 'has_image', label: 'Has image' },
		{ value: 'time_range', label: 'UTC time range' },
		{ value: 'text', label: 'Indexed text' },
		{ value: 'workflow', label: 'Review / bookmark' },
		{ value: 'producer_ids', label: 'Embedding producer' },
		{ value: 'model_ids', label: 'Embedding model' }
	];

	function createPredicate(kind: EventExpressionPredicate['kind']): EventExpressionPredicate {
		switch (kind) {
			case 'camera_ids':
				return { kind, values: cameras[0] ? [cameras[0].id] : [''] };
			case 'group_ids':
				return { kind, values: [''] };
			case 'event_types':
				return { kind, values: ['person'] };
			case 'origins':
				return { kind, values: ['keeppeek'] };
			case 'zones':
				return { kind, values: [''] };
			case 'missing_zone':
				return { kind };
			case 'confidence':
				return { kind, operator: 'at_least', threshold: 0.7 };
			case 'missing_confidence':
				return { kind };
			case 'has_image':
				return { kind };
			case 'time_range': {
				const endMs = Date.now();
				return { kind, startMs: endMs - 3_600_000, endMs };
			}
			case 'text':
				return { kind, prefix: '' };
			case 'workflow':
				return { kind, state: 'reviewed' };
			case 'producer_ids':
				return { kind, values: [''] };
			case 'model_ids':
				return { kind, values: [''] };
		}
	}

	function updateAtPath(
		root: EventExpressionNode,
		path: readonly number[],
		updater: (node: EventExpressionNode) => EventExpressionNode | null
	): EventExpressionNode {
		if (path.length === 0) {
			return updater(root) ?? { op: 'and', children: [] };
		}
		const [head, ...rest] = path;
		if (root.op === 'not') {
			const nextChild = root.child ? updateAtPath(root.child, rest, updater) : null;
			return {
				op: 'not',
				child: rest.length === 0 ? (root.child ? updater(root.child) : null) : nextChild
			};
		}
		if (root.op === 'and' || root.op === 'or') {
			const nextChildren: EventExpressionNode[] = [];
			root.children.forEach((child, index) => {
				if (index !== head) {
					nextChildren.push(child);
					return;
				}
				if (rest.length === 0) {
					const updated = updater(child);
					if (updated !== null) nextChildren.push(updated);
				} else {
					nextChildren.push(updateAtPath(child, rest, updater));
				}
			});
			return { op: root.op, children: nextChildren };
		}
		return root;
	}

	function moveChild(path: readonly number[], index: number, delta: -1 | 1): void {
		csvDrafts = {};
		emitDraft(
			updateAtPath(draft, path, (node) => {
				if (node.op !== 'and' && node.op !== 'or') return node;
				const target = index + delta;
				if (target < 0 || target >= node.children.length) return node;
				const children = [...node.children];
				const [moved] = children.splice(index, 1);
				if (moved) children.splice(target, 0, moved);
				return { op: node.op, children };
			})
		);
	}

	function addCondition(path: readonly number[]): void {
		emitDraft(
			updateAtPath(draft, path, (node) => {
				const leaf: EventExpressionNode = {
					op: 'predicate',
					predicate: defaultExpressionPredicate()
				};
				if (node.op === 'and' || node.op === 'or') {
					return { op: node.op, children: [...node.children, leaf] };
				}
				if (node.op === 'not') {
					return { op: 'not', child: leaf };
				}
				return { op: 'and', children: [node, leaf] };
			})
		);
	}

	function addGroup(path: readonly number[], op: 'and' | 'or' | 'not'): void {
		emitDraft(
			updateAtPath(draft, path, (node) => {
				const group: EventExpressionNode =
					op === 'not'
						? { op: 'not', child: { op: 'predicate', predicate: defaultExpressionPredicate() } }
						: { op, children: [{ op: 'predicate', predicate: defaultExpressionPredicate() }] };
				if (node.op === 'and' || node.op === 'or') {
					return { op: node.op, children: [...node.children, group] };
				}
				if (node.op === 'not') {
					return { op: 'not', child: group };
				}
				return { op: 'and', children: [node, group] };
			})
		);
	}

	function removeAt(path: readonly number[]): void {
		csvDrafts = {};
		emitDraft(updateAtPath(draft, path, () => null));
	}

	function changeGroupOp(path: readonly number[], nextOp: 'and' | 'or' | 'not'): void {
		csvDrafts = {};
		emitDraft(
			updateAtPath(draft, path, (node) => {
				if (nextOp === 'not') {
					const first =
						node.op === 'and' || node.op === 'or'
							? (node.children[0] ?? null)
							: node.op === 'not'
								? node.child
								: node;
					return { op: 'not', child: first };
				}
				const children =
					node.op === 'and' || node.op === 'or'
						? node.children
						: node.op === 'not'
							? node.child
								? [node.child]
								: []
							: [node];
				return { op: nextOp, children };
			})
		);
	}

	function updatePredicate(path: readonly number[], predicate: EventExpressionPredicate): void {
		emitDraft(updateAtPath(draft, path, () => ({ op: 'predicate', predicate })));
	}

	function csvKey(path: readonly number[], kind: EventExpressionPredicate['kind']): string {
		return `${path.join('.')}:${kind}`;
	}

	function updateCsvInput(
		path: readonly number[],
		kind: 'camera_ids' | 'group_ids' | 'event_types' | 'zones' | 'producer_ids' | 'model_ids',
		raw: string
	): void {
		csvDrafts = { ...csvDrafts, [csvKey(path, kind)]: raw };
		updatePredicate(path, { kind, values: splitCsv(raw) });
	}

	function clearCsvDraft(path: readonly number[], kind: EventExpressionPredicate['kind']): void {
		const key = csvKey(path, kind);
		if (!(key in csvDrafts)) return;
		const next = { ...csvDrafts };
		delete next[key];
		csvDrafts = next;
	}

	function formatUtcDateTimeLocal(ms: number): string {
		if (!Number.isFinite(ms)) return '';
		return new Date(ms).toISOString().slice(0, 16);
	}

	function parseUtcDateTimeLocal(raw: string, fallbackMs: number): number {
		const trimmed = raw.trim();
		if (!trimmed) return fallbackMs;
		const parsed = Date.parse(trimmed.endsWith('Z') ? trimmed : `${trimmed}Z`);
		return Number.isFinite(parsed) ? parsed : fallbackMs;
	}

	function splitCsv(raw: string): string[] {
		return raw
			.split(',')
			.map((value) => value.trim())
			.filter((value) => value.length > 0);
	}
</script>

{#snippet renderNode(
	node: EventExpressionNode,
	path: number[],
	depth: number,
	siblingIndex: number,
	siblingCount: number,
	parentPath: number[] | null
)}
	{#if node.op === 'and' || node.op === 'or' || node.op === 'not'}
		<fieldset
			class="space-y-2 rounded-sm border border-hairline bg-ground/60 p-2.5"
			data-expression-group={node.op}
		>
			<legend class="sr-only">{node.op.toUpperCase()} expression group at depth {depth}</legend>
			<div class="flex flex-wrap items-center gap-2">
				<label class="flex items-center gap-1.5 text-xs font-medium">
					<span class="sr-only">Group operator</span>
					<select
						aria-label={`Group operator at level ${depth}`}
						value={node.op}
						class="h-8 rounded-sm border border-hairline bg-raised px-2 font-mono text-xs"
						onchange={(event) =>
							changeGroupOp(path, event.currentTarget.value as 'and' | 'or' | 'not')}
					>
						<option value="and">Match ALL (AND)</option>
						<option value="or">Match ANY (OR)</option>
						<option value="not">Exclude (NOT)</option>
					</select>
				</label>
				{#if node.op !== 'not' || node.child === null}
					<button
						type="button"
						class="inline-flex h-8 items-center gap-1 rounded-sm border border-hairline bg-surface px-2 text-xs hover:bg-raised"
						onclick={() => addCondition(path)}
					>
						<PlusIcon class="size-3.5" /> Add condition
					</button>
				{/if}
				{#if depth < MAX_EXPRESSION_DEPTH && (node.op !== 'not' || node.child === null)}
					<button
						type="button"
						class="inline-flex h-8 items-center gap-1 rounded-sm border border-hairline bg-surface px-2 text-xs hover:bg-raised"
						onclick={() => addGroup(path, 'or')}
					>
						<PlusIcon class="size-3.5" /> Add group
					</button>
				{/if}
				{#if parentPath !== null}
					<button
						type="button"
						class="grid size-8 place-items-center rounded-sm border border-hairline bg-surface hover:bg-raised disabled:opacity-40"
						aria-label="Move group up"
						disabled={siblingIndex === 0}
						onclick={() => moveChild(parentPath, siblingIndex, -1)}
					>
						<ArrowUpIcon class="size-3.5" />
					</button>
					<button
						type="button"
						class="grid size-8 place-items-center rounded-sm border border-hairline bg-surface hover:bg-raised disabled:opacity-40"
						aria-label="Move group down"
						disabled={siblingIndex >= siblingCount - 1}
						onclick={() => moveChild(parentPath, siblingIndex, 1)}
					>
						<ArrowDownIcon class="size-3.5" />
					</button>
					<button
						type="button"
						class="grid size-8 place-items-center rounded-sm border border-hairline bg-surface text-destructive hover:bg-raised"
						aria-label="Remove group"
						onclick={() => removeAt(path)}
					>
						<Trash2Icon class="size-3.5" />
					</button>
				{/if}
			</div>

			<div class="space-y-2 border-l-2 border-hairline pl-3">
				{#if node.op === 'not'}
					{#if node.child}
						{@render renderNode(node.child, [...path, 0], depth + 1, 0, 1, path)}
					{:else}
						<p class="text-xs text-destructive">Add one condition or group to exclude.</p>
					{/if}
				{:else if node.children.length === 0}
					<p class="text-xs text-destructive"
						>This group is empty. Add a condition before applying.</p
					>
				{:else}
					{#each node.children as child, index (index)}
						{@render renderNode(
							child,
							[...path, index],
							depth + 1,
							index,
							node.children.length,
							path
						)}
					{/each}
				{/if}
			</div>
		</fieldset>
	{:else}
		<div
			class="flex flex-wrap items-center gap-2 rounded-sm border border-hairline bg-surface p-2"
			data-expression-predicate={node.predicate.kind}
		>
			<select
				aria-label="Predicate field"
				value={node.predicate.kind}
				class="h-8 rounded-sm border border-hairline bg-raised px-2 text-xs"
				onchange={(event) => {
					clearCsvDraft(path, node.predicate.kind);
					updatePredicate(
						path,
						createPredicate(event.currentTarget.value as EventExpressionPredicate['kind'])
					);
				}}
			>
				{#each predicateKinds as item (item.value)}
					<option value={item.value}>{item.label}</option>
				{/each}
			</select>

			{#if node.predicate.kind === 'camera_ids' || node.predicate.kind === 'group_ids' || node.predicate.kind === 'event_types' || node.predicate.kind === 'zones' || node.predicate.kind === 'producer_ids' || node.predicate.kind === 'model_ids'}
				{@const listPred = node.predicate}
				{@const fieldKey = csvKey(path, listPred.kind)}
				<input
					type="text"
					aria-label={`${listPred.kind} values`}
					value={csvDrafts[fieldKey] ?? listPred.values.join(', ')}
					placeholder="Comma-separated values"
					class="h-8 min-w-44 flex-1 rounded-sm border border-hairline bg-raised px-2 text-xs"
					oninput={(event) => updateCsvInput(path, listPred.kind, event.currentTarget.value)}
					onblur={() => clearCsvDraft(path, listPred.kind)}
				/>
			{:else if node.predicate.kind === 'time_range'}
				{@const rangePred = node.predicate}
				<input
					type="datetime-local"
					aria-label="UTC start time"
					value={formatUtcDateTimeLocal(rangePred.startMs)}
					class="h-8 rounded-sm border border-hairline bg-raised px-2 font-mono text-xs"
					onchange={(event) =>
						updatePredicate(path, {
							...rangePred,
							startMs: parseUtcDateTimeLocal(event.currentTarget.value, rangePred.startMs)
						})}
				/>
				<span class="text-xs text-muted-foreground">to</span>
				<input
					type="datetime-local"
					aria-label="UTC end time"
					value={formatUtcDateTimeLocal(rangePred.endMs)}
					class="h-8 rounded-sm border border-hairline bg-raised px-2 font-mono text-xs"
					onchange={(event) =>
						updatePredicate(path, {
							...rangePred,
							endMs: parseUtcDateTimeLocal(event.currentTarget.value, rangePred.endMs)
						})}
				/>
				<span class="font-mono text-[11px] text-muted-foreground">UTC</span>
			{:else if node.predicate.kind === 'origins'}
				{@const originPred = node.predicate}
				<select
					aria-label="Event origin value"
					value={originPred.values[0] ?? 'keeppeek'}
					class="h-8 rounded-sm border border-hairline bg-raised px-2 text-xs"
					onchange={(event) =>
						updatePredicate(path, {
							kind: 'origins',
							values: [event.currentTarget.value as 'camera' | 'keeppeek']
						})}
				>
					<option value="keeppeek">KeepPeek pipeline</option>
					<option value="camera">Camera native</option>
				</select>
			{:else if node.predicate.kind === 'confidence'}
				{@const confPred = node.predicate}
				<select
					aria-label="Confidence operator"
					value={confPred.operator}
					class="h-8 rounded-sm border border-hairline bg-raised px-2 text-xs"
					onchange={(event) =>
						updatePredicate(path, {
							...confPred,
							operator: event.currentTarget.value as 'at_least' | 'at_most'
						})}
				>
					<option value="at_least">at least (&gt;=)</option>
					<option value="at_most">at most (&lt;=)</option>
				</select>
				<input
					type="number"
					aria-label="Confidence threshold"
					min="0"
					max="1"
					step="0.05"
					value={confPred.threshold}
					class="h-8 w-24 rounded-sm border border-hairline bg-raised px-2 font-mono text-xs"
					onchange={(event) =>
						updatePredicate(path, {
							...confPred,
							threshold: event.currentTarget.valueAsNumber
						})}
				/>
			{:else if node.predicate.kind === 'text'}
				{@const textPred = node.predicate}
				<select
					aria-label="Indexed text field"
					value={textPred.field ?? ''}
					class="h-8 rounded-sm border border-hairline bg-raised px-2 text-xs"
					onchange={(event) =>
						updatePredicate(path, {
							...textPred,
							field: (event.currentTarget.value || undefined) as EventTextField | undefined
						})}
				>
					<option value="">Any field</option>
					<option value="event_type">Event type</option>
					<option value="face_name">Face name</option>
					<option value="object_class">Object class</option>
					<option value="text">Text</option>
				</select>
				<input
					type="text"
					aria-label="Indexed text prefix"
					value={textPred.prefix}
					placeholder="Prefix..."
					class="h-8 min-w-36 flex-1 rounded-sm border border-hairline bg-raised px-2 text-xs"
					oninput={(event) =>
						updatePredicate(path, { ...textPred, prefix: event.currentTarget.value })}
				/>
			{:else if node.predicate.kind === 'workflow'}
				{@const wfPred = node.predicate}
				<select
					aria-label="Workflow state"
					value={wfPred.state}
					class="h-8 rounded-sm border border-hairline bg-raised px-2 text-xs"
					onchange={(event) =>
						updatePredicate(path, {
							kind: 'workflow',
							state: event.currentTarget.value as EventWorkflowStatePredicate
						})}
				>
					<option value="reviewed">Reviewed</option>
					<option value="dismissed">Dismissed</option>
					<option value="bookmarked">Bookmarked</option>
					<option value="bookmarked_by_me">Bookmarked by me</option>
				</select>
			{/if}

			{#if parentPath !== null}
				<button
					type="button"
					class="grid size-8 place-items-center rounded-sm border border-hairline bg-surface hover:bg-raised disabled:opacity-40"
					aria-label="Move condition up"
					disabled={siblingIndex === 0}
					onclick={() => moveChild(parentPath, siblingIndex, -1)}
				>
					<ArrowUpIcon class="size-3.5" />
				</button>
				<button
					type="button"
					class="grid size-8 place-items-center rounded-sm border border-hairline bg-surface hover:bg-raised disabled:opacity-40"
					aria-label="Move condition down"
					disabled={siblingIndex >= siblingCount - 1}
					onclick={() => moveChild(parentPath, siblingIndex, 1)}
				>
					<ArrowDownIcon class="size-3.5" />
				</button>
				<button
					type="button"
					class="grid size-8 place-items-center rounded-sm border border-hairline bg-surface text-destructive hover:bg-raised"
					aria-label="Remove condition"
					onclick={() => removeAt(path)}
				>
					<Trash2Icon class="size-3.5" />
				</button>
			{/if}
		</div>
	{/if}
{/snippet}

<section
	class="space-y-3 rounded-md border border-hairline bg-surface p-3"
	aria-label="Advanced boolean event search builder"
	data-testid="event-expression-builder"
>
	<div class="flex flex-wrap items-center justify-between gap-2">
		<div>
			<h2 class="text-sm font-semibold">Advanced boolean search</h2>
			<p class="text-xs text-text-muted" data-testid="event-expression-summary">
				Effective query: {summaryText}
			</p>
		</div>
		<div class="flex flex-wrap items-center gap-2">
			{#if onconvert}
				<button
					type="button"
					class="h-8 rounded-sm border border-hairline bg-raised px-2.5 text-xs hover:bg-surface disabled:opacity-50"
					disabled={!canConvertToCommon}
					title={canConvertToCommon
						? 'Convert this expression back to common filters'
						: 'Cannot convert to common filters without losing nested boolean semantics'}
					onclick={onconvert}
				>
					Use common filters
				</button>
			{/if}
			<button
				type="button"
				class="h-8 rounded-sm bg-primary px-3 text-xs font-medium text-on-primary disabled:opacity-50"
				disabled={validationError !== null}
				onclick={onapply}
			>
				Run advanced search
			</button>
		</div>
	</div>

	{#if validationError}
		<p
			role="alert"
			class="rounded-sm border border-destructive/40 bg-destructive/10 px-2.5 py-1.5 text-xs text-destructive"
			data-testid="event-expression-error"
		>
			{validationError}
		</p>
	{/if}

	{@render renderNode(draft, [], 1, 0, 1, null)}

	{#if !semanticAvailable}
		<p class="text-xs text-text-muted" data-testid="semantic-fallback-notice">
			Semantic ranking is currently unavailable. Structured boolean and common filters continue
			working normally.
		</p>
	{/if}

	{#if semanticSummary}
		<div
			class="rounded-sm border border-hairline bg-ground px-2.5 py-2 text-xs text-text-muted"
			data-testid="semantic-provenance-summary"
		>
			<p class="font-medium text-foreground">
				Semantic ranking evidence (similarity score only, not authoritative identity)
			</p>
			<p>
				Model: {semanticSummary.modelId}{semanticSummary.modelVersion
					? `@${semanticSummary.modelVersion}`
					: ''} · Producer: {semanticSummary.producerId} · {semanticSummary.dimensions}d · Score:
				{semanticSummary.scoreInterpretation}
				{#if semanticSummary.minimumScore !== undefined}
					· Min score: {semanticSummary.minimumScore.toFixed(2)}
				{/if}
			</p>
			<p>
				Searched interval: {new Date(semanticSummary.searchedStartMs).toISOString()} to
				{new Date(semanticSummary.searchedEndMs).toISOString()} · Candidates:
				{semanticSummary.candidateCount}{semanticSummary.candidatesTruncated ? ' (truncated)' : ''}
			</p>
		</div>
	{/if}

	{#if diagnostics}
		<div
			class="rounded-sm border border-hairline bg-ground px-2.5 py-1.5 font-mono text-2xs text-text-muted"
			data-testid="planner-diagnostics"
		>
			Plan: {diagnostics.planCategory} · est={diagnostics.estimatedCandidates} · scanned={diagnostics.scannedCandidates}
			· {diagnostics.wallTimeMs}ms
		</div>
	{/if}
</section>
