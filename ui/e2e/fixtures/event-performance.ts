import { expect, type Page } from '@playwright/test';

type EventPerformanceState = {
	firstPageMs: number | null;
	activeObjectUrls: Set<string>;
	longTasks: number[];
};

type EventPerformanceWindow = Window & { __eventPerformance: EventPerformanceState };

export async function openBuiltEventsPage(page: Page, eventDate: string) {
	// The deployed first-page budget excludes Vite development compilation.
	const backendPort = process.env.KEEPPEEK_E2E_BACKEND_PORT ?? '4317';
	const response = await page.goto(`http://127.0.0.1:${backendPort}/events?date=${eventDate}`);
	if (!response) throw new Error('The built Events page did not return a response');
	expect(response.status()).toBe(200);
	expect(await response.text()).toContain('/_app/immutable/');
}

export async function installEventPerformance(page: Page) {
	await page.addInitScript(() => {
		const state: EventPerformanceState = {
			firstPageMs: null,
			activeObjectUrls: new Set<string>(),
			longTasks: []
		};
		(window as unknown as EventPerformanceWindow).__eventPerformance = state;
		const createObjectUrl = URL.createObjectURL.bind(URL);
		const revokeObjectUrl = URL.revokeObjectURL.bind(URL);
		URL.createObjectURL = (object) => {
			const url = createObjectUrl(object);
			state.activeObjectUrls.add(url);
			return url;
		};
		URL.revokeObjectURL = (url) => {
			state.activeObjectUrls.delete(url);
			revokeObjectUrl(url);
		};
		new PerformanceObserver((list) => {
			state.longTasks.push(...list.getEntries().map((entry) => entry.duration));
		}).observe({ type: 'longtask', buffered: true });
		let frameId = 0;
		const observer = new MutationObserver(() => {
			if (frameId || document.querySelectorAll('[data-event-card]').length !== 18) return;
			// Allow one paint before recording the visible page, independently of runner polling.
			frameId = requestAnimationFrame(() => {
				frameId = requestAnimationFrame(() => {
					frameId = 0;
					const cards = [...document.querySelectorAll('[data-event-card]')];
					if (
						cards.length !== 18 ||
						!cards.every((card) =>
							card.checkVisibility({ visibilityProperty: true, opacityProperty: true })
						)
					) {
						return;
					}
					state.firstPageMs = performance.now();
					cleanup();
				});
			});
		});
		const cleanup = () => {
			observer.disconnect();
			cancelAnimationFrame(frameId);
			clearTimeout(timeoutId);
			window.removeEventListener('pagehide', cleanup);
		};
		const timeoutId = setTimeout(cleanup, 10_000);
		window.addEventListener('pagehide', cleanup, { once: true });
		observer.observe(document, { childList: true, subtree: true, attributes: true });
	});
}

export async function readEventPerformance(page: Page) {
	await page.waitForFunction(
		() => (window as unknown as EventPerformanceWindow).__eventPerformance.firstPageMs !== null,
		null,
		{ timeout: 10_000 }
	);
	return page.evaluate(() => {
		const state = (window as unknown as EventPerformanceWindow).__eventPerformance;
		if (state.firstPageMs === null) throw new Error('The first Events page did not render');
		return {
			firstPageMs: state.firstPageMs,
			activeObjectUrls: state.activeObjectUrls.size,
			maxLongTaskMs: Math.max(0, ...state.longTasks),
			eventCards: document.querySelectorAll('[data-event-card]').length
		};
	});
}
