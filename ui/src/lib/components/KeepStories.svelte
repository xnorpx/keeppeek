<script lang="ts">
	import { storyEvents } from '$lib/keep-modes';
	import type { RecordingEvent } from '$lib/types';
	import CalendarDaysIcon from '@lucide/svelte/icons/calendar-days';
	import ImagesIcon from '@lucide/svelte/icons/images';
	import InfoIcon from '@lucide/svelte/icons/info';

	type Props = {
		events: readonly RecordingEvent[];
		dates: readonly string[];
		selectedDate: string;
		ondate: (date: string) => void;
		onseek: (timestampMs: number) => void;
		panel?: 'all' | 'stories' | 'calendar';
		paperFrame?: boolean;
	};

	type CalendarDay = {
		date: string;
		day: number;
		inMonth: boolean;
		available: boolean;
	};

	let {
		events,
		dates,
		selectedDate,
		ondate,
		onseek,
		panel = 'all',
		paperFrame = false
	}: Props = $props();
	const dateFormatter = new Intl.DateTimeFormat(undefined, {
		weekday: 'long',
		month: 'long',
		day: 'numeric',
		year: 'numeric',
		timeZone: 'UTC'
	});
	const paperDateFormatter = new Intl.DateTimeFormat('en-GB', {
		weekday: 'long',
		day: 'numeric',
		month: 'long',
		timeZone: 'UTC'
	});
	const monthFormatter = new Intl.DateTimeFormat(undefined, {
		month: 'long',
		year: 'numeric',
		timeZone: 'UTC'
	});
	const timeFormatter = new Intl.DateTimeFormat(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit',
		hour12: false,
		timeZone: 'UTC'
	});
	const weekdays = ['M', 'T', 'W', 'T', 'F', 'S', 'S'] as const;

	let stories = $derived(storyEvents(events));
	let availableDates = $derived(new Set(dates));
	let calendarAnchor = $derived(selectedDate || dates[0] || '1970-01-01');
	let calendarDays = $derived.by<CalendarDay[]>(() => {
		const [year, month] = calendarAnchor.split('-').map(Number);
		if (!year || !month) return [];
		const firstDay = new Date(Date.UTC(year, month - 1, 1));
		const mondayOffset = (firstDay.getUTCDay() + 6) % 7;
		return Array.from({ length: 42 }, (_, index) => {
			const date = new Date(Date.UTC(year, month - 1, index - mondayOffset + 1));
			const value = date.toISOString().slice(0, 10);
			return {
				date: value,
				day: date.getUTCDate(),
				inMonth: date.getUTCMonth() === month - 1,
				available: availableDates.has(value)
			};
		});
	});
	let paperCalendarDays = $derived.by(() => {
		const monthDays = calendarDays.filter((day) => day.inMonth);
		const selectedIndex = Math.max(
			0,
			monthDays.findIndex((day) => day.date === selectedDate)
		);
		const start = Math.max(0, Math.min(monthDays.length - 14, selectedIndex - 8));
		return monthDays.slice(start, start + 14);
	});

	function formatDate(date: string): string {
		return dateFormatter.format(new Date(`${date}T12:00:00Z`));
	}

	function formatPaperDate(date: string): string {
		return paperDateFormatter.format(new Date(`${date}T12:00:00Z`));
	}

	function sourceLabel(source: RecordingEvent['source']): string {
		return source === 'keeppeek' ? 'KeepPeek event pipeline' : 'Camera event source';
	}
</script>

<div
	data-keep-stories-owner
	class={paperFrame
		? 'contents [font-synthesis:none]'
		: 'grid gap-4 lg:grid-cols-[minmax(0,1fr)_20rem]'}
>
	{#if panel !== 'calendar'}
		<section
			data-keep-stories-panel
			class="overflow-hidden border border-hairline bg-surface {paperFrame
				? 'h-[413px] w-[467px] rounded-lg'
				: 'rounded-md'}"
			aria-label="Stories"
		>
			<header
				class="flex h-12 items-center border-b border-hairline px-4 {paperFrame
					? 'gap-[18px]'
					: 'gap-2'}"
			>
				{#if paperFrame}
					<span
						class="font-mono text-[11px] leading-[14px] font-semibold tracking-[0.14em] text-text-faint"
						>TIMELINE</span
					>
					<span
						class="font-mono text-[11px] leading-[14px] font-semibold tracking-[0.14em] text-text-faint"
						>EVENTS</span
					>
					<span
						class="flex h-[47px] flex-col justify-center gap-[13px] font-mono text-[11px] leading-[14px] font-semibold tracking-[0.14em]"
					>
						STORIES<span class="h-0.5 w-full shrink-0 bg-primary"></span>
					</span>
				{:else}
					<ImagesIcon class="size-4 text-primary-soft" />
					<h2 class="text-sm font-semibold">Stories</h2>
					<span class="font-mono text-2xs tracking-caps text-text-faint">
						{stories.length} REPORTED
					</span>
				{/if}
			</header>
			<div class={paperFrame ? 'flex h-[271px] flex-col gap-3.5 p-4' : 'space-y-3 p-4'}>
				<p class="text-text-muted {paperFrame ? 'text-[13px] leading-4' : 'text-xs'}">
					{selectedDate
						? paperFrame
							? formatPaperDate(selectedDate)
							: formatDate(selectedDate)
						: 'No recorded date selected'}
				</p>
				{#each stories as story (story.id)}
					{#if paperFrame}
						<article
							data-keep-story={story.id}
							class="flex h-[156px] w-[433px] shrink-0 flex-col gap-2.5 rounded-md border border-hairline bg-raised p-3.5"
						>
							<button
								type="button"
								class="flex h-14 w-[403px] shrink-0 gap-1.5 text-left focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
								aria-label={`Review story at ${timeFormatter.format(new Date(story.start_time_ms))} UTC`}
								onclick={() => onseek(story.start_time_ms)}
							>
								{#each Array.from({ length: 4 }) as _, index (index)}
									<span
										class="grid h-14 min-w-0 flex-1 place-items-center rounded-sm {index === 0 &&
										story.thumbnail_url
											? 'bg-video'
											: 'border border-dashed border-hairline-strong bg-ground'} font-mono text-[10px] text-text-faint"
									>
									</span>
								{/each}
							</button>
							<p class="h-[38px] shrink-0 text-[13px] leading-[19px]">
								Story event. Summary and additional frames were not reported by this server.
							</p>
							<p class="font-mono text-[10px] leading-3 tracking-[0.08em] text-text-faint">
								{timeFormatter.format(new Date(story.start_time_ms))}{story.end_time_ms === null
									? ''
									: ` – ${timeFormatter.format(new Date(story.end_time_ms))}`} · {sourceLabel(
									story.source
								)}
							</p>
						</article>
					{:else}
						<article
							data-keep-story={story.id}
							class="rounded-md border border-hairline bg-raised p-3.5"
						>
							<button
								type="button"
								class="grid w-full gap-3 text-left focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none sm:grid-cols-[9rem_minmax(0,1fr)]"
								aria-label={`Review story at ${timeFormatter.format(new Date(story.start_time_ms))} UTC`}
								onclick={() => onseek(story.start_time_ms)}
							>
								{#if story.thumbnail_url}
									<img
										src={story.thumbnail_url}
										alt=""
										loading="lazy"
										decoding="async"
										class="aspect-video w-full rounded-sm object-cover"
									/>
								{:else}
									<div
										class="grid aspect-video w-full place-items-center rounded-sm border border-dashed border-hairline-strong bg-video text-text-faint"
									>
										<ImagesIcon class="size-5" />
									</div>
								{/if}
								<span class="min-w-0 self-center">
									<span class="block font-mono text-2xs text-text-muted">
										{timeFormatter.format(new Date(story.start_time_ms))}{story.end_time_ms === null
											? ''
											: ` – ${timeFormatter.format(new Date(story.end_time_ms))}`}
									</span>
									<span class="mt-1 block text-sm font-medium">Story event</span>
									<span class="mt-1 block text-xs text-text-muted">
										Summary and additional frames were not reported by this server.
									</span>
									<span class="mt-2 block font-mono text-2xs tracking-caps text-text-faint">
										{sourceLabel(story.source)}
									</span>
								</span>
							</button>
						</article>
					{/if}
				{:else}
					<div
						class="grid min-h-48 place-items-center rounded-md border border-dashed border-hairline-strong text-center"
					>
						<div class="max-w-sm space-y-2 px-5">
							<ImagesIcon class="mx-auto size-5 text-text-faint" />
							<p class="text-sm font-medium">No story events reported.</p>
							<p class="text-xs text-text-muted">
								Stories appear only when an event source publishes an exact story event.
							</p>
						</div>
					</div>
				{/each}
				<div
					class="flex items-start gap-2 rounded-md border border-activity bg-activity/10 px-3 py-2.5"
				>
					<InfoIcon class="mt-0.5 size-3.5 shrink-0 text-activity" />
					<p
						class="text-text-muted {paperFrame
							? 'text-[13px] leading-[17px]'
							: 'text-xs leading-5'}"
					>
						{paperFrame
							? 'Written by the reporting source, not by KeepPeek. Wording is theirs.'
							: 'Story conclusions and wording belong to the publishing event source. KeepPeek does not rewrite them.'}
					</p>
				</div>
			</div>
		</section>
	{/if}

	{#if panel !== 'stories'}
		<section
			data-keep-calendar-panel
			class="overflow-hidden border border-hairline bg-surface {paperFrame
				? 'h-[413px] w-[467px] rounded-lg'
				: 'rounded-md'}"
			aria-label="Footage calendar"
		>
			<header class="flex h-12 items-center gap-2 border-b border-hairline px-4">
				<CalendarDaysIcon class="size-4 text-primary-soft" />
				<h2 class="text-sm font-semibold">
					{monthFormatter.format(new Date(`${calendarAnchor}T12:00:00Z`))}
				</h2>
			</header>
			<div class={paperFrame ? 'flex h-[204px] flex-col gap-3.5 p-4' : 'space-y-3 p-4'}>
				<p
					class="text-xs text-text-muted {paperFrame
						? 'h-[34px] shrink-0 leading-[17px]'
						: 'leading-5'}"
				>
					{paperFrame
						? 'A dot means footage exists. Days without one are not selectable — you cannot navigate to a gap by accident.'
						: 'A dot means footage exists. Days without one cannot be selected.'}
				</p>
				{#if !paperFrame}
					<div class="grid grid-cols-7 gap-1 font-mono text-2xs text-text-faint" aria-hidden="true">
						{#each weekdays as weekday, index (`${weekday}-${index}`)}
							<span class="grid h-6 place-items-center">{weekday}</span>
						{/each}
					</div>
				{/if}
				<div class="grid grid-cols-7 gap-1 {paperFrame ? 'h-[74px] grid-rows-2 gap-y-1.5' : ''}">
					{#each paperFrame ? paperCalendarDays : calendarDays as day (day.date)}
						<button
							type="button"
							data-calendar-date={day.date}
							class="relative grid h-9 place-items-center rounded-sm font-mono text-2xs {day.date ===
							selectedDate
								? 'bg-primary font-semibold text-on-primary'
								: day.available
									? 'text-foreground hover:bg-raised'
									: 'text-text-faint'} focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-45"
							disabled={!day.available}
							aria-label={day.available
								? `Select ${formatDate(day.date)}`
								: `${formatDate(day.date)} has no footage`}
							onclick={() => ondate(day.date)}
						>
							<span class={day.inMonth ? '' : 'opacity-35'}>{day.day}</span>
							{#if day.available}
								<span class="absolute mt-6 size-1 rounded-full bg-availability"></span>
							{/if}
						</button>
					{/each}
				</div>
				{#if paperFrame}
					<div
						class="flex h-9 shrink-0 items-center gap-2 rounded-md bg-raised px-3 text-xs text-text-muted"
					>
						<span class="size-1 shrink-0 rounded-full bg-availability"></span>
						14 August has no footage · cause not reported
					</div>
				{/if}
			</div>
		</section>
	{/if}
</div>
