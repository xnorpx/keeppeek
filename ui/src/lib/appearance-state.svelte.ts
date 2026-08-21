import {
	normalizeThemePreference,
	resolveEffectiveTheme,
	type EffectiveTheme,
	type ThemePreference
} from '$lib/appearance-system';

export class AppearanceState {
	preference = $state<ThemePreference>('dark');
	effectiveTheme = $state<EffectiveTheme>('dark');
	#media: MediaQueryList | null = null;

	initialize(): () => void {
		this.#media = window.matchMedia('(prefers-color-scheme: dark)');
		let storedPreference: string | null = null;
		try {
			storedPreference = localStorage.getItem('keeppeek-theme');
		} catch {
			storedPreference = document.documentElement.dataset.themePreference ?? null;
		}
		this.#apply(normalizeThemePreference(storedPreference), false);

		const followSystem = () => {
			if (this.preference === 'system') this.#apply('system', false);
		};
		this.#media.addEventListener('change', followSystem);
		return () => {
			this.#media?.removeEventListener('change', followSystem);
			this.#media = null;
		};
	}

	setPreference(preference: ThemePreference): void {
		this.#apply(preference, true);
	}

	toggleEffectiveTheme(): void {
		this.setPreference(this.effectiveTheme === 'dark' ? 'light' : 'dark');
	}

	#apply(preference: ThemePreference, persist: boolean): void {
		const effectiveTheme = resolveEffectiveTheme(
			preference,
			this.#media?.matches ?? document.documentElement.classList.contains('dark')
		);
		this.preference = preference;
		this.effectiveTheme = effectiveTheme;
		document.documentElement.classList.toggle('dark', effectiveTheme === 'dark');
		document.documentElement.dataset.theme = effectiveTheme;
		document.documentElement.dataset.themePreference = preference;
		if (!persist) return;
		try {
			localStorage.setItem('keeppeek-theme', preference);
		} catch {
			// The active theme still applies when storage is unavailable.
		}
	}
}
