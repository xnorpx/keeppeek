import '../local-preview.css';
import type { Preview } from '@storybook/svelte';
import { installStorybookReadinessGuard, type LokiReadyStateHost } from '../storybook-readiness';

if (typeof document !== 'undefined') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';

	installStorybookReadinessGuard(window as typeof window & LokiReadyStateHost);
}

const preview: Preview = {
	parameters: {
		layout: 'fullscreen',
		options: { storySort: { order: ['Foundation'] } },
		controls: { expanded: true },
		a11y: { test: 'error' }
	}
};

export default preview;
