<script lang="ts">
	import Plus from '@lucide/svelte/icons/plus';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import type { SourceChoice } from './connection-manager';
	import { maxSources } from './config';

	type Props = {
		config: Record<string, unknown>;
		cameras: readonly SourceChoice[];
		message: string | null;
		onchange: (config: Record<string, unknown>) => void;
		ondiscover: () => void;
	};
	let { config, cameras, message, onchange, ondiscover }: Props = $props();
	let sources = $derived(
		(Array.isArray(config.sources) ? config.sources : []) as Record<string, unknown>[]
	);

	function patch(field: string, value: unknown) {
		onchange({ ...config, [field]: value });
	}
	function stringValue(value: unknown): string {
		return typeof value === 'string' ? value : '';
	}
	function updateSource(index: number, field: string, value: string) {
		patch(
			'sources',
			sources.map((source, sourceIndex) =>
				sourceIndex === index
					? { ...source, [field]: field === 'title' && !value ? undefined : value }
					: source
			)
		);
	}
	function replaceToken(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		if (input.value) patch('token', input.value);
		input.value = '';
	}
</script>

<div class="editor">
	<label for="endpoint">KeepPeek endpoint</label>
	<input
		id="endpoint"
		type="url"
		maxlength="2048"
		autocomplete="off"
		value={stringValue(config.endpoint)}
		oninput={(event) => patch('endpoint', event.currentTarget.value)}
	/>
	<label for="access-key">Access key</label>
	<div class="input-action">
		<input
			id="access-key"
			type="password"
			maxlength="512"
			autocomplete="new-password"
			placeholder={config.token ? 'Configured (unchanged)' : 'Not configured'}
			onchange={replaceToken}
		/>
		<button
			type="button"
			class="icon-button"
			aria-label="Clear access key"
			title="Clear access key"
			onclick={() => patch('token', '')}><Trash2 size={18} /></button
		>
	</div>
	<label for="card-title">Card title</label>
	<input
		id="card-title"
		maxlength="160"
		value={stringValue(config.title)}
		oninput={(event) => patch('title', event.currentTarget.value || undefined)}
	/>
	<fieldset class="layout-options">
		<legend>Layout</legend>
		<label
			><input
				type="radio"
				name="layout"
				checked={config.layout !== 'single'}
				onchange={() => patch('layout', 'grid')}
			/>Grid</label
		>
		<label
			><input
				type="radio"
				name="layout"
				checked={config.layout === 'single'}
				onchange={() => patch('layout', 'single')}
			/>Single camera</label
		>
	</fieldset>
	<div class="form-grid">
		<label
			>Columns
			<select
				value={String(config.columns ?? 2)}
				onchange={(event) => patch('columns', Number(event.currentTarget.value))}
			>
				{#each [1, 2, 3, 4] as columns (columns)}<option value={String(columns)}>{columns}</option
					>{/each}
			</select>
		</label>
		<label
			>Aspect ratio
			<select
				value={String(config.aspect_ratio ?? '16:9')}
				onchange={(event) => patch('aspect_ratio', event.currentTarget.value)}
			>
				{#each ['16:9', '4:3', '1:1'] as ratio (ratio)}<option value={ratio}>{ratio}</option>{/each}
			</select>
		</label>
	</div>
	<label class="checkbox"
		><input
			type="checkbox"
			checked={config.show_name !== false}
			onchange={(event) => patch('show_name', event.currentTarget.checked)}
		/>Show camera names</label
	>
	<div class="editor-heading">
		<h3>Cameras</h3>
		<button type="button" onclick={ondiscover}><RefreshCw size={16} />Load sources</button>
	</div>
	{#if message}<p class="error" role="alert">{message}</p>{/if}
	<datalist id="source-choices">
		{#each cameras as camera (camera.source_id)}<option value={camera.source_id}
				>{camera.title}{camera.available ? '' : ' (offline)'}</option
			>{/each}
	</datalist>
	{#each sources as source, index (index)}
		<fieldset class="source-editor">
			<legend>Camera {index + 1}</legend>
			<div class="input-action">
				<label
					>Source ID<input
						list="source-choices"
						maxlength="160"
						value={stringValue(source.source_id)}
						oninput={(event) => updateSource(index, 'source_id', event.currentTarget.value)}
					/></label
				>
				<button
					type="button"
					class="icon-button"
					aria-label={`Remove camera ${index + 1}`}
					title="Remove camera"
					onclick={() =>
						patch(
							'sources',
							sources.filter((_, sourceIndex) => sourceIndex !== index)
						)}><Trash2 size={18} /></button
				>
			</div>
			<div class="form-grid">
				<label
					>Camera title<input
						maxlength="160"
						value={stringValue(source.title)}
						oninput={(event) => updateSource(index, 'title', event.currentTarget.value)}
					/></label
				>
				<label
					>Video quality
					<select
						value={String(source.quality ?? 'auto')}
						onchange={(event) => updateSource(index, 'quality', event.currentTarget.value)}
					>
						<option value="auto">Automatic</option><option value="low">Low</option><option
							value="high">High</option
						>
					</select>
				</label>
			</div>
		</fieldset>
	{/each}
	<button
		type="button"
		disabled={sources.length >= maxSources}
		onclick={() => patch('sources', [...sources, { source_id: '', quality: 'auto' }])}
		><Plus size={16} />Add camera</button
	>
</div>
