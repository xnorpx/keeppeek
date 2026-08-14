<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';

	onMount(() => {
		void goto(resolve('/keep'), { replaceState: true });
	});
</script>

<svelte:head>
	<title>Keep - KeepPeek</title>
</svelte:head>
