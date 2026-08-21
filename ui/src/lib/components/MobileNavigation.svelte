<script lang="ts">
	import { resolve } from '$app/paths';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import BellIcon from '@lucide/svelte/icons/bell';
	import ClockIcon from '@lucide/svelte/icons/clock-3';
	import MoreHorizontalIcon from '@lucide/svelte/icons/ellipsis';
	import VideoIcon from '@lucide/svelte/icons/video';

	type Props = {
		pathname: string;
		fixed?: boolean;
	};

	let { pathname, fixed = true }: Props = $props();

	const items = [
		{ href: resolve('/'), label: 'Peek', icon: VideoIcon, paths: ['/'] },
		{ href: resolve('/keep'), label: 'Keep', icon: ClockIcon, paths: ['/keep', '/recordings'] },
		{ href: resolve('/events'), label: 'Events', icon: BellIcon, paths: ['/events'] },
		{
			href: resolve('/system-health'),
			label: 'Health',
			icon: ActivityIcon,
			paths: ['/system-health']
		},
		{ href: resolve('/settings'), label: 'More', icon: MoreHorizontalIcon, paths: ['/settings'] }
	] as const;

	function matchesRoute(paths: readonly string[]): boolean {
		return paths.some((path) =>
			path === '/' ? pathname === path : pathname === path || pathname.startsWith(`${path}/`)
		);
	}
</script>

<nav
	data-shell-mobile-nav
	class="{fixed
		? 'fixed inset-x-0 bottom-0 z-50'
		: 'relative'} grid h-[78px] shrink-0 grid-cols-5 border-t border-sidebar-border bg-sidebar pt-2.5 pb-6 text-sidebar-foreground md:hidden"
	aria-label="Primary navigation"
>
	{#each items as item (item.href)}
		{@const active = matchesRoute(item.paths)}
		<a
			href={item.href}
			aria-current={active ? 'page' : undefined}
			class="flex min-w-0 flex-col items-center justify-center {active
				? 'gap-[5px] text-xs leading-[14px] font-semibold text-primary-soft'
				: 'gap-1 text-2xs leading-3 text-text-faint'}"
		>
			<item.icon class="size-[18px]" strokeWidth={1.7} />
			<span>{item.label}</span>
		</a>
	{/each}
</nav>
