import { mount, tick, unmount } from 'svelte';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import CopyMomentLink from './CopyMomentLink.svelte';

const components: Array<ReturnType<typeof mount>> = [];
const targets: HTMLElement[] = [];
const link =
	'https://keeppeek.example/keep?camera=front&date=2026-09-05&at=1788652800000&stream=auto';
const commandName = 'Copy link to this moment (sign-in required)';

function render(getLink = vi.fn(() => link)) {
	const target = document.createElement('div');
	document.body.append(target);
	targets.push(target);
	components.push(mount(CopyMomentLink, { target, props: { getLink } }));
	return { target, getLink };
}

afterEach(async () => {
	for (const component of components.splice(0)) await unmount(component);
	for (const target of targets.splice(0)) target.remove();
	vi.restoreAllMocks();
});

describe('Copy recording-moment link', () => {
	it('snapshots on invocation and announces success without navigation or scroll changes', async () => {
		const write = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue();
		const { getLink } = render();
		expect(getLink).not.toHaveBeenCalled();
		const before = { url: location.href, history: history.length, scroll: scrollY };
		await page.getByRole('button', { name: commandName }).click();
		expect(getLink).toHaveBeenCalledTimes(1);
		expect(write).toHaveBeenCalledWith(link);
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('Recording link copied. Sign-in required.');
		expect({ url: location.href, history: history.length, scroll: scrollY }).toEqual(before);
	});

	it('shows a read-only selected fallback link after clipboard denial and restores focus', async () => {
		vi.spyOn(navigator.clipboard, 'writeText').mockRejectedValue(
			new DOMException('Denied', 'NotAllowedError')
		);
		const { target, getLink } = render();
		await page.getByRole('button', { name: commandName }).click();
		await expect.element(page.getByRole('dialog', { name: 'Copy recording link' })).toBeVisible();
		const input = page.getByLabelText('Authenticated recording link');
		await expect.element(input).toHaveValue(link);
		await expect.element(input).toHaveAttribute('readonly');
		await expect.element(input).toHaveFocus();
		expect(target.querySelector<HTMLInputElement>('input')?.selectionStart).toBe(0);
		expect(target.querySelector<HTMLInputElement>('input')?.selectionEnd).toBe(link.length);
		expect(getLink).toHaveBeenCalledTimes(1);
		await page.getByRole('button', { name: 'Close copy dialog' }).click();
		await expect.element(page.getByRole('button', { name: commandName })).toHaveFocus();
	});

	it('provides the same accessible fallback when Clipboard API is unavailable', async () => {
		vi.spyOn(navigator, 'clipboard', 'get').mockReturnValue(undefined as unknown as Clipboard);
		render();
		await page.getByRole('button', { name: commandName }).click();
		await expect.element(page.getByLabelText('Authenticated recording link')).toHaveValue(link);
	});

	it('never exposes errors or copies a link when no moment can be resolved', async () => {
		const write = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue();
		render(
			vi.fn(() => {
				throw new Error('private-debug-value');
			})
		);
		await page.getByRole('button', { name: commandName }).click();
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('A recording moment is not available to copy.');
		expect(write).not.toHaveBeenCalled();
		expect(document.body.textContent).not.toContain('private-debug-value');
	});
});

describe('Copy recording-moment link lifecycle', () => {
	it('bounds stalled writes, ignores repeat clicks, and retains the original fallback snapshot', async () => {
		const writeResult = Promise.withResolvers<void>();
		const write = vi.spyOn(navigator.clipboard, 'writeText').mockReturnValue(writeResult.promise);
		const { getLink } = render();
		const button = page.getByRole('button', { name: commandName });
		await button.click();
		await button.click();
		expect(getLink).toHaveBeenCalledTimes(1);
		expect(write).toHaveBeenCalledTimes(1);
		await expect.element(page.getByLabelText('Authenticated recording link')).toHaveValue(link);
		getLink.mockReturnValue(`${link}&mode=stories`);
		writeResult.resolve();
		await tick();
		await expect.element(page.getByLabelText('Authenticated recording link')).toHaveValue(link);
		await expect.element(button).toHaveAttribute('aria-busy', 'false');
	});

	it('ignores a clipboard completion after its control is destroyed', async () => {
		const writeResult = Promise.withResolvers<void>();
		vi.spyOn(navigator.clipboard, 'writeText').mockReturnValue(writeResult.promise);
		const { target } = render();
		await page.getByRole('button', { name: commandName }).click();
		await unmount(components.pop()!);
		writeResult.resolve();
		await tick();
		expect(target.childElementCount).toBe(0);
	});
});
