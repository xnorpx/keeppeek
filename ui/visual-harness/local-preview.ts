import './local-preview.css';
import { mount } from 'svelte';
import Board04KeepTimelineStory from './stories/Board04KeepTimelineStory.svelte';
import Board06PeekLiveWallStory from './stories/Board06PeekLiveWallStory.svelte';
import Board07CameraPageStory from './stories/Board07CameraPageStory.svelte';
import Board08LayoutEditorStory from './stories/Board08LayoutEditorStory.svelte';
import Board08LayoutRegistryStory from './stories/Board08LayoutRegistryStory.svelte';
import Board09KeepModesStory from './stories/Board09KeepModesStory.svelte';
import Board10EventsStory from './stories/Board10EventsStory.svelte';
import Board11CameraFleetStory from './stories/Board11CameraFleetStory.svelte';
import Board12AddCameraStory from './stories/Board12AddCameraStory.svelte';
import Board13StorageRetentionStory from './stories/Board13StorageRetentionStory.svelte';
import Board14EventSourcesStory from './stories/Board14EventSourcesStory.svelte';
import Board15HealthStory from './stories/Board15HealthStory.svelte';
import Board16AccessStory from './stories/Board16AccessStory.svelte';
import Board17IntegrationsStory from './stories/Board17IntegrationsStory.svelte';
import Board18NotificationsStory from './stories/Board18NotificationsStory.svelte';
import Board19GroupsStory from './stories/Board19GroupsStory.svelte';
import Board20AppearanceSystemStory from './stories/Board20AppearanceSystemStory.svelte';
import Board21FirstRunStory from './stories/Board21FirstRunStory.svelte';
import Board33StatesStory from './stories/Board33StatesStory.svelte';
import Board31HistoryStory from './stories/Board31HistoryStory.svelte';
import DemoHistoryStory from './stories/DemoHistoryStory.svelte';
import DemoCameraCatalogWizardStory from './stories/DemoCameraCatalogWizardStory.svelte';
import ExportLifecycleStory from './stories/ExportLifecycleStory.svelte';
import LightThemePeekStory from './stories/LightThemePeekStory.svelte';
import Board27MobileAdministrationStory from './stories/Board27MobileAdministrationStory.svelte';
import Board26MobileHealthStory from './stories/Board26MobileHealthStory.svelte';
import Board26MobileDiagnosisStory from './stories/Board26MobileDiagnosisStory.svelte';
import Board30CameraDiagnosisStory from './stories/Board30CameraDiagnosisStory.svelte';
import Board25MobileAddCameraStory from './stories/Board25MobileAddCameraStory.svelte';
import Board24MobileCameraStory from './stories/Board24MobileCameraStory.svelte';
import Board23CameraConfigurationStory from './stories/Board23CameraConfigurationStory.svelte';
import Board22MobileMediaStory from './stories/Board22MobileMediaStory.svelte';

const target = document.querySelector('#app');
if (!(target instanceof HTMLElement)) throw new Error('Visual preview target is unavailable');

declare global {
	interface Window {
		__keepPeekDemoStart?: () => void | Promise<void>;
	}
}

const previewUrl = new URL(window.location.href);
const scenarioId = previewUrl.searchParams.get('scenario');
const demoMode = previewUrl.searchParams.get('demo') === 'true';
const demoAssetId = previewUrl.searchParams.get('demoAsset');
if (demoMode) {
	document.documentElement.dataset.demo = 'true';
	if (demoAssetId) document.documentElement.dataset.demoAsset = demoAssetId;
}
const board33States = {
	'peek.waiting.first-keyframe': 'first-keyframe',
	'keep.waiting.cold-seek': 'cold-seek',
	'cameras.waiting.discovery': 'discovery',
	'events.empty.no-results': 'no-results',
	'settings.waiting.applying': 'applying',
	'cameras.waiting.fleet-skeleton': 'fleet-skeleton'
} as const;

if (scenarioId === 'keep.desktop.timeline-anatomy') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board04KeepTimelineStory, { target });
} else if (scenarioId === 'peek.desktop.live-wall') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board06PeekLiveWallStory, { target });
} else if (scenarioId === 'camera.desktop.details-ptz') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board07CameraPageStory, { target });
} else if (scenarioId === 'peek.desktop.layout-editor') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board08LayoutEditorStory, { target });
} else if (scenarioId === 'peek.desktop.layout-registry') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board08LayoutRegistryStory, { target });
} else if (scenarioId === 'keep.desktop.stories') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board09KeepModesStory, { target, props: { state: 'stories' } });
} else if (scenarioId === 'keep.desktop.calendar') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board09KeepModesStory, { target, props: { state: 'calendar' } });
} else if (scenarioId === 'keep.desktop.export-gated') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board09KeepModesStory, { target, props: { state: 'export' } });
} else if (scenarioId === 'keep.desktop.swimlanes') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board09KeepModesStory, { target, props: { state: 'swimlanes' } });
} else if (scenarioId === 'events.desktop.browse') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board10EventsStory, { target, props: { state: 'browse' } });
} else if (scenarioId === 'events.desktop.detail') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board10EventsStory, { target, props: { state: 'detail' } });
} else if (scenarioId === 'cameras.desktop.fleet') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board11CameraFleetStory, { target });
} else if (scenarioId === 'cameras.desktop.add-wizard') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	if (demoMode && demoAssetId === 'cameras.desktop.catalog-guided-setup') {
		mount(DemoCameraCatalogWizardStory, { target });
	} else {
		mount(Board12AddCameraStory, { target });
	}
} else if (scenarioId === 'settings.desktop.storage-retention') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board13StorageRetentionStory, { target });
} else if (scenarioId === 'settings.desktop.event-sources') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board14EventSourcesStory, { target });
} else if (scenarioId === 'health.desktop.overview') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board15HealthStory, { target });
} else if (scenarioId === 'settings.desktop.access') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board16AccessStory, { target });
} else if (scenarioId === 'settings.desktop.integrations') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board17IntegrationsStory, { target });
} else if (scenarioId === 'settings.desktop.notifications') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board18NotificationsStory, { target });
} else if (scenarioId === 'groups.desktop.administration') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board19GroupsStory, { target, props: { state: 'administration' } });
} else if (scenarioId === 'groups.desktop.participant') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board19GroupsStory, { target, props: { state: 'participant' } });
} else if (scenarioId === 'settings.desktop.appearance-system-logs') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board20AppearanceSystemStory, { target });
} else if (scenarioId === 'setup.desktop.first-run') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board21FirstRunStory, { target });
} else if (scenarioId === 'peek.mobile.live') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board22MobileMediaStory, { target, props: { state: 'peek' } });
} else if (scenarioId === 'keep.mobile.timeline') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board22MobileMediaStory, { target, props: { state: 'keep' } });
} else if (scenarioId === 'events.mobile.browse') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board22MobileMediaStory, { target, props: { state: 'events' } });
} else if (scenarioId === 'camera.desktop.configuration') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board23CameraConfigurationStory, { target });
} else if (scenarioId === 'camera.mobile.details-ptz') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board24MobileCameraStory, { target, props: { mode: 'live' } });
} else if (scenarioId === 'camera.mobile.ptz') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board24MobileCameraStory, { target, props: { mode: 'ptz' } });
} else if (scenarioId === 'camera.mobile.settings') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board24MobileCameraStory, { target, props: { mode: 'settings' } });
} else if (scenarioId === 'cameras.mobile.add-wizard') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board25MobileAddCameraStory, { target, props: { stage: 'find-connect' } });
} else if (scenarioId === 'cameras.mobile.add-streams') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board25MobileAddCameraStory, { target, props: { stage: 'streams' } });
} else if (scenarioId === 'cameras.mobile.add-review') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board25MobileAddCameraStory, { target, props: { stage: 'review' } });
} else if (scenarioId === 'health.mobile.overview') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board26MobileHealthStory, { target });
} else if (scenarioId === 'health.mobile.camera-issue') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board26MobileDiagnosisStory, { target, props: { state: 'issue' } });
} else if (scenarioId === 'health.mobile.stream-evidence') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board26MobileDiagnosisStory, { target, props: { state: 'stream' } });
} else if (scenarioId === 'settings.mobile.administration') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board27MobileAdministrationStory, { target, props: { state: 'index' } });
} else if (scenarioId === 'settings.mobile.access') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board27MobileAdministrationStory, { target, props: { state: 'access' } });
} else if (scenarioId === 'health.desktop.camera-diagnosis') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board30CameraDiagnosisStory, { target });
} else if (scenarioId === 'peek.desktop.focus-history') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board31HistoryStory, { target, props: { state: 'focused' } });
} else if (scenarioId === 'peek.desktop.history-keep') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	if (demoMode) mount(DemoHistoryStory, { target });
	else mount(Board31HistoryStory, { target, props: { state: 'keep' } });
} else if (scenarioId === 'peek.desktop.light-theme') {
	document.documentElement.classList.remove('dark');
	document.documentElement.dataset.theme = 'light';
	mount(LightThemePeekStory, { target });
} else if (scenarioId !== null && scenarioId in board33States) {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(Board33StatesStory, {
		target,
		props: { state: board33States[scenarioId as keyof typeof board33States] }
	});
} else if (scenarioId === null || scenarioId === 'keep.desktop.export-lifecycle') {
	document.documentElement.classList.add('dark');
	document.documentElement.dataset.theme = 'dark';
	mount(ExportLifecycleStory, { target });
} else {
	throw new Error(`Unknown local visual scenario: ${scenarioId}`);
}

if (demoMode) void signalDemoStart();

async function signalDemoStart(): Promise<void> {
	await document.fonts.ready;
	await new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()));
	await new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()));
	document.documentElement.dataset.demoReady = 'true';
	await window.__keepPeekDemoStart?.();
}
