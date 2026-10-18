import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createRawSnippet } from 'svelte';
import { render } from 'vitest-browser-svelte';
import Layout from './+layout.svelte';

const isolation = vi.hoisted(() => ({ normalImported: vi.fn(), beforeNavigate: vi.fn() }));
vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/share') } }));
vi.mock('$app/navigation', () => ({ beforeNavigate: isolation.beforeNavigate }));
vi.mock('$lib/components/AppShell.svelte', () => {
	isolation.normalImported();
	return { default: null };
});
const originalUrl = window.location.href;
afterEach(() => window.history.replaceState(window.history.state, '', originalUrl));

describe('Exact guest root layout isolation', () => {
	it('renders guest children without importing the ordinary shell or its logger/control contexts', async () => {
		window.history.replaceState(window.history.state, '', '/share');
		const children = createRawSnippet(() => ({ render: () => '<p>Isolated guest child</p>' }));
		await render(Layout, { props: { children } });
		await expect.element(browserPage.getByText('Isolated guest child')).toBeVisible();
		expect(isolation.normalImported).not.toHaveBeenCalled();
		expect(isolation.beforeNavigate).toHaveBeenCalledTimes(1);
		expect(document.querySelector('[data-shell-rail]')).toBeNull();
	});
});
