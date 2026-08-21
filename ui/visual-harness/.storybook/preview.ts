import '../local-preview.css';
import type { Preview } from '@storybook/svelte';

if (typeof document !== 'undefined') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
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
