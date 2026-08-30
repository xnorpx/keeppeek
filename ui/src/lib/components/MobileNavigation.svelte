<script lang="ts">
	import { resolve } from '$app/paths';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import BellIcon from '@lucide/svelte/icons/bell';
	import ClockIcon from '@lucide/svelte/icons/clock-3';
	import EyeIcon from '@lucide/svelte/icons/eye';
	import LayoutDashboardIcon from '@lucide/svelte/icons/layout-dashboard';
	import MoreHorizontalIcon from '@lucide/svelte/icons/ellipsis';

	type Props = {
		pathname: string;
		administrator?: boolean;
		fixed?: boolean;
	};

	let { pathname, administrator = true, fixed = true }: Props = $props();

	const allItems = [
		{
			href: resolve('/'),
			label: 'Dashboard',
			icon: LayoutDashboardIcon,
			paths: ['/'],
			administrator: false
		},
		{
			href: resolve('/viewer'),
			label: 'Viewer',
			icon: EyeIcon,
			paths: ['/viewer'],
			administrator: false
		},
		{
			href: resolve('/keep'),
			label: 'Keep',
			icon: ClockIcon,
			paths: ['/keep', '/recordings'],
			administrator: false
		},
		{
			href: resolve('/events'),
			label: 'Events',
			icon: BellIcon,
			paths: ['/events'],
			administrator: false
		},
		{
			href: resolve('/system-health'),
			label: 'Health',
			icon: ActivityIcon,
			paths: ['/system-health'],
			administrator: true
		},
		{
			href: resolve('/settings'),
			label: 'More',
			icon: MoreHorizontalIcon,
			paths: ['/settings'],
			administrator: true
		}
	] as const;
	let items = $derived(allItems.filter((item) => administrator || !item.administrator));

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
		: 'relative'} grid h-[78px] shrink-0 {administrator
		? 'grid-cols-6'
		: 'grid-cols-4'} border-t border-sidebar-border bg-sidebar pt-2.5 pb-6 text-sidebar-foreground md:hidden"
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
