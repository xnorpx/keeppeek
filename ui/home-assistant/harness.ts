import '@fontsource/archivo/400.css';
import '@fontsource/archivo/600.css';
import { mount } from 'svelte';
import Plus from '@lucide/svelte/icons/plus';
import Trash2 from '@lucide/svelte/icons/trash-2';
import Settings2 from '@lucide/svelte/icons/settings-2';
import X from '@lucide/svelte/icons/x';
import '../src/lib/home-assistant/elements.svelte';
import { parseCardConfig, type CardConfig } from '../src/lib/home-assistant/config';
import './harness.css';

type LovelaceElement = HTMLElement & { setConfig: (value: unknown) => void };

class DashboardHarness {
	#cards: Array<{ element: LovelaceElement; config: CardConfig }> = [];
	#defaultConfig: CardConfig;
	#dashboard = document.querySelector<HTMLElement>('#dashboard')!;
	#dialog = document.querySelector<HTMLDialogElement>('#settings')!;
	#error = document.querySelector<HTMLElement>('#error')!;

	constructor(config: CardConfig) {
		this.#defaultConfig = config;
		document.querySelector('#add')!.addEventListener('click', () => this.add());
		document.querySelector('#remove')!.addEventListener('click', () => this.remove());
		document.querySelector('#edit')!.addEventListener('click', () => this.edit());
		document.querySelector('#close')!.addEventListener('click', () => this.#dialog.close());
		this.#dialog.addEventListener('close', () =>
			document.querySelector('#editor')!.replaceChildren()
		);
		document.querySelector<HTMLInputElement>('#theme')!.addEventListener('change', (event) => {
			document.body.classList.toggle('dark', (event.currentTarget as HTMLInputElement).checked);
		});
		for (const [id, Icon] of [
			['add', Plus],
			['remove', Trash2],
			['edit', Settings2],
			['close', X]
		] as const) {
			const icon = document.createElement('span');
			document.getElementById(id)!.prepend(icon);
			mount(Icon, { target: icon, props: { size: 16 } });
		}
		this.add();
	}

	private add(): void {
		if (this.#cards.length >= 8) return;
		const config = this.#cards[0]?.config ?? this.#defaultConfig;
		const element = document.createElement('keeppeek-card') as LovelaceElement;
		element.setConfig(config);
		this.#cards.push({ element, config });
		this.#dashboard.append(element);
		this.buttons();
	}

	private remove(): void {
		this.#cards.pop()?.element.remove();
		this.buttons();
	}

	private buttons(): void {
		document.querySelector<HTMLButtonElement>('#add')!.disabled = this.#cards.length >= 8;
		for (const id of ['remove', 'edit'])
			document.querySelector<HTMLButtonElement>(`#${id}`)!.disabled = this.#cards.length === 0;
	}

	private edit(): void {
		const card = this.#cards[0];
		if (!card) return;
		const editor = document.createElement('keeppeek-card-editor') as LovelaceElement;
		editor.setConfig(card.config);
		editor.addEventListener('config-changed', (event) => {
			const value = (event as CustomEvent<{ config: unknown }>).detail.config;
			try {
				card.element.setConfig(value);
				card.config = parseCardConfig(value);
				this.#error.textContent = '';
			} catch {
				this.#error.textContent = 'Card configuration is incomplete.';
			}
		});
		document.querySelector('#editor')!.replaceChildren(editor);
		this.#dialog.showModal();
	}
}

async function initialize(): Promise<void> {
	try {
		const response = await fetch('/__keeppeek_fixture', { cache: 'no-store' });
		if (!response.ok) throw new Error('Fixture unavailable.');
		new DashboardHarness(parseCardConfig(await response.json()));
	} catch {
		document.querySelector('#error')!.textContent =
			'The local camera fixture is unavailable. Start the Home Assistant demo command.';
	}
}

if (typeof document !== 'undefined') void initialize();
