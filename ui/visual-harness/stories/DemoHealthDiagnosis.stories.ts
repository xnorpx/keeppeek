import type { Meta, StoryObj } from '@storybook/svelte';
import { healthDiagnosisDemo } from '../demo-scenarios-ops';
import DemoHealthDiagnosisStory from './DemoHealthDiagnosisStory.svelte';

const meta = {
	title: 'Demos/Camera Health',
	component: DemoHealthDiagnosisStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records server-authoritative health triage on /system-health, drilling into Back Yard for missing transport evidence, then switching Porch from UDP to TCP.'
			}
		}
	}
} satisfies Meta<typeof DemoHealthDiagnosisStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const TriageAndDiagnosis: Story = {
	parameters: {
		paper: healthDiagnosisDemo.metadata.paper,
		demo: healthDiagnosisDemo.metadata.demo
	}
};
