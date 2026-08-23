import type { Meta, StoryObj } from '@storybook/svelte';
import { cameraCatalogWizardDemo } from '../demo-scenarios';
import DemoCameraCatalogWizardStory from './DemoCameraCatalogWizardStory.svelte';

const meta = {
	title: 'Demos/Camera Catalog Wizard',
	component: DemoCameraCatalogWizardStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records the catalog-guided setup path, including discovery enrichment, manual model search, the outbound source link, and review before the first configuration write.'
			}
		}
	}
} satisfies Meta<typeof DemoCameraCatalogWizardStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const GuidedSetup: Story = {
	parameters: {
		paper: cameraCatalogWizardDemo.metadata.paper,
		demo: cameraCatalogWizardDemo.metadata.demo
	}
};
