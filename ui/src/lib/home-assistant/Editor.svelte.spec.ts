import { mount, unmount } from 'svelte';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import Editor from './Editor.svelte';

const mounted: Array<ReturnType<typeof mount>> = [];
const targets: HTMLElement[] = [];
const config = {
	type: 'custom:keeppeek-card',
	endpoint: 'https://keeppeek.example.net',
	token: 'private-fixture-access-key',
	title: 'Cameras',
	sources: [{ source_id: 'front-door', title: 'Entrance', quality: 'high' }],
	layout: 'grid',
	columns: 2,
	aspect_ratio: '16:9',
	show_name: true,
	grid_options: { columns: 12 }
};

function editor() {
	const onchange = vi.fn();
	const target = document.createElement('div');
	document.body.append(target);
	targets.push(target);
	mounted.push(
		mount(Editor, {
			target,
			props: {
				config,
				onchange,
				cameras: [],
				message: null,
				ondiscover: vi.fn()
			}
		})
	);
	return { onchange, target };
}

afterEach(async () => {
	for (const component of mounted.splice(0)) await unmount(component);
	for (const target of targets.splice(0)) target.remove();
});

describe('KeepPeek Lovelace visual editor', () => {
	it('shows the configured column count on initial render', async () => {
		const { target } = editor();
		await vi.waitFor(() =>
			expect(target.querySelector<HTMLSelectElement>('select')?.value).toBe('2')
		);
	});

	it('retains credentials and Home Assistant layout metadata when editing the title', async () => {
		const { onchange, target } = editor();
		await page.getByLabelText('Card title').fill('Entry cameras');
		expect(onchange).toHaveBeenLastCalledWith({ ...config, title: 'Entry cameras' });
		expect(target.innerHTML).not.toContain(config.token);
		expect(target.querySelector<HTMLInputElement>('input[type="password"]')?.value).toBe('');
	});

	it('round-trips source titles and quality when changing the column count', async () => {
		const { onchange } = editor();
		await page.getByLabelText('Columns').selectOptions('3');
		expect(onchange).toHaveBeenLastCalledWith({ ...config, columns: 3 });
	});

	it('allows explicit credential replacement without rendering the previous value', async () => {
		const { onchange, target } = editor();
		await page.getByLabelText('Access key', { exact: true }).fill('replacement-fixture-key');
		await page.getByLabelText('Card title').click();
		expect(onchange).toHaveBeenCalledWith({ ...config, token: 'replacement-fixture-key' });
		expect(target.innerHTML).not.toContain(config.token);
		expect(target.querySelector<HTMLInputElement>('input[type="password"]')?.value).toBe('');
	});
});
