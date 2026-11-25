'use strict';

const form = document.getElementById('connect-form');
const serverUrlInput = document.getElementById('server-url');
const saveDefaultCheckbox = document.getElementById('save-default');
const connectButton = document.getElementById('connect-button');
const retryButton = document.getElementById('retry-button');
const statusMessage = document.getElementById('status-message');
const errorMessage = document.getElementById('error-message');
const configMeta = document.getElementById('config-meta');

let busy = false;
let navigationGuardTimer = null;
const NAVIGATION_TIMEOUT_MS = 10_000;

function clearNavigationGuard() {
	if (navigationGuardTimer !== null) {
		window.clearTimeout(navigationGuardTimer);
		navigationGuardTimer = null;
	}
}

function invokeCommand(command, args = {}) {
	const bridge = window.__TAURI_INTERNALS__;
	if (!bridge || typeof bridge.invoke !== 'function') {
		return Promise.reject(new Error('Desktop IPC bridge is unavailable on this origin.'));
	}
	return bridge.invoke(command, args);
}

function setBusy(nextBusy, message = '') {
	busy = nextBusy;
	form.setAttribute('aria-busy', String(nextBusy));
	serverUrlInput.disabled = nextBusy;
	saveDefaultCheckbox.disabled = nextBusy;
	connectButton.disabled = nextBusy;
	retryButton.disabled = nextBusy;
	connectButton.textContent = nextBusy ? 'Connecting...' : 'Connect';
	if (message) {
		statusMessage.textContent = message;
		statusMessage.hidden = false;
	} else {
		statusMessage.hidden = true;
		statusMessage.textContent = '';
	}
}

function showError(message) {
	if (message) {
		errorMessage.textContent = message;
		errorMessage.hidden = false;
	} else {
		errorMessage.hidden = true;
		errorMessage.textContent = '';
	}
}

async function attemptConnect(saveDefault, fromStartup = false) {
	if (busy) return;
	const candidate = serverUrlInput.value.trim();
	if (!candidate) {
		showError('Enter a KeepPeek server URL such as http://127.0.0.1:8081.');
		serverUrlInput.focus();
		return;
	}

	showError('');
	clearNavigationGuard();
	setBusy(true, `Checking ${candidate}...`);
	try {
		await invokeCommand('connect_server', {
			serverUrl: candidate,
			saveDefault
		});
		setBusy(true, `Opening ${candidate}...`);
		navigationGuardTimer = window.setTimeout(() => {
			navigationGuardTimer = null;
			setBusy(false);
			showError(
				`The desktop webview did not finish loading ${candidate}. Verify the server is reachable and click Retry.`
			);
			serverUrlInput.focus();
		}, NAVIGATION_TIMEOUT_MS);
	} catch (failure) {
		clearNavigationGuard();
		setBusy(false);
		const message = failure instanceof Error ? failure.message : String(failure);
		showError(message);
		if (fromStartup) {
			void invokeCommand('record_launcher_probe_failure').catch(() => undefined);
		}
		serverUrlInput.focus();
	}
}

async function initializeLauncher() {
	const params = new URLSearchParams(window.location.search);
	const manualMode = params.get('manual') === '1';

	try {
		const state = await invokeCommand('get_desktop_state');
		serverUrlInput.value = state.server_url;
		configMeta.textContent = `Configuration file: ${state.config_path} (${state.os}/${state.arch})`;
		if (state.last_error) {
			showError(state.last_error);
		}
		if (state.auto_connect && !manualMode) {
			await attemptConnect(false, true);
		} else {
			serverUrlInput.focus();
		}
	} catch (failure) {
		const message = failure instanceof Error ? failure.message : String(failure);
		showError(`KeepPeek Desktop could not initialize: ${message}`);
	}
}

form.addEventListener('submit', (event) => {
	event.preventDefault();
	void attemptConnect(saveDefaultCheckbox.checked, false);
});

retryButton.addEventListener('click', () => {
	void attemptConnect(false, false);
});

window.addEventListener('pagehide', clearNavigationGuard);

void initializeLauncher();
