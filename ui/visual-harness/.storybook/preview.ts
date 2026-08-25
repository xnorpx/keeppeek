import '../local-preview.css';
import type { Preview } from '@storybook/svelte';
import {
	waitForStorybookIndexBeforeExtract,
	type StorybookPreviewRuntime
} from '../storybook-readiness';

if (typeof document !== 'undefined') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';

	const storybookPreview = (
		window as typeof window & { __STORYBOOK_PREVIEW__?: StorybookPreviewRuntime }
	).__STORYBOOK_PREVIEW__;
	waitForStorybookIndexBeforeExtract(storybookPreview);
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
