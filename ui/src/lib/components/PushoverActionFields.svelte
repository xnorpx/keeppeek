<script lang="ts">
	import { createPushoverConfig } from '$lib/notifications';
	import type {
		NotificationRuleDefinition,
		PushoverPriority,
		PushoverPublicConfig
	} from '$lib/notifications';
	import { Input } from '$lib/components/ui/input/index.js';

	type NotificationAction = NotificationRuleDefinition['actions'][number];
	type PushoverSecret = 'application_token' | 'user_key';
	type Props = {
		action: NotificationAction;
		secret: (field: PushoverSecret) => string;
		onsecret: (field: PushoverSecret, value: string) => void;
		onconfig: (field: keyof PushoverPublicConfig, value: string | number | null) => void;
	};

	let { action, secret, onsecret, onconfig }: Props = $props();
	let config = $derived(action.pushover ?? createPushoverConfig());
</script>

<label class="space-y-1.5 text-xs font-medium"
	>Application token<Input
		type="password"
		autocomplete="new-password"
		minlength={30}
		maxlength={30}
		value={secret('application_token')}
		placeholder={action.destination_configured ? 'Configured' : ''}
		oninput={(event) => onsecret('application_token', event.currentTarget.value)}
		required={!action.destination_configured}
	/></label
>
<label class="space-y-1.5 text-xs font-medium"
	>User or group key<Input
		type="password"
		autocomplete="new-password"
		minlength={30}
		maxlength={30}
		value={secret('user_key')}
		placeholder={action.destination_configured ? 'Configured' : ''}
		oninput={(event) => onsecret('user_key', event.currentTarget.value)}
		required={!action.destination_configured}
	/></label
>
<label class="space-y-1.5 text-xs font-medium"
	>Device names<Input
		value={config.device ?? ''}
		oninput={(event) => onconfig('device', event.currentTarget.value || null)}
		placeholder="All devices"
	/></label
>
<label class="space-y-1.5 text-xs font-medium"
	>Sound<Input
		value={config.sound ?? ''}
		oninput={(event) => onconfig('sound', event.currentTarget.value || null)}
		placeholder="Account default"
	/></label
>
<label class="space-y-1.5 text-xs font-medium"
	>Priority<select
		value={config.priority}
		onchange={(event) =>
			onconfig('priority', Number(event.currentTarget.value) as PushoverPriority)}
		class="h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
		><option value={-2}>Lowest</option><option value={-1}>Low</option><option value={0}
			>Normal</option
		><option value={1}>High</option><option value={2}>Emergency</option></select
	></label
>
<label class="space-y-1.5 text-xs font-medium"
	>Deep-link base URL<Input
		type="url"
		value={config.deep_link_base_url ?? ''}
		oninput={(event) => onconfig('deep_link_base_url', event.currentTarget.value || null)}
		placeholder="https://keeppeek.example/"
	/></label
>
{#if config.priority === 2}
	<label class="space-y-1.5 text-xs font-medium"
		>Emergency retry (seconds)<Input
			type="number"
			min="30"
			value={config.retry_seconds ?? 30}
			oninput={(event) => onconfig('retry_seconds', Number(event.currentTarget.value))}
			required
		/></label
	>
	<label class="space-y-1.5 text-xs font-medium"
		>Emergency expiry (seconds)<Input
			type="number"
			min="1"
			max="10800"
			value={config.expire_seconds ?? 300}
			oninput={(event) => onconfig('expire_seconds', Number(event.currentTarget.value))}
			required
		/></label
	>
{/if}
