<script lang="ts">
	import { setCapabilityState } from '$lib/capability-context';
	import GroupsSection from '$lib/components/GroupsSection.svelte';

	type State = 'administration' | 'participant';
	type Props = { state: State };

	let { state }: Props = $props();
	setCapabilityState();

	const scenarioIds: Record<State, string> = {
		administration: 'groups.desktop.administration',
		participant: 'groups.desktop.participant'
	};
</script>

<main
	data-paper-scenario={scenarioIds[state]}
	class="w-[1440px] overflow-hidden bg-ground [font-synthesis:none] {state === 'administration'
		? 'h-[416px]'
		: 'h-[420px]'}"
>
	<GroupsSection paperFrame={state} />
</main>
