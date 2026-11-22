import { expect, type Page } from '@playwright/test';

type Append = { source: string; buffer: number; mime: string; mode: string; box: string };
type Probe = { appends: Append[]; overflow: boolean };
type ProbeWindow = Window & { recordedAudioMseProbe: Probe };

function installProbe() {
	const probe: Probe = { appends: [], overflow: false };
	(window as unknown as ProbeWindow).recordedAudioMseProbe = probe;
	const sources = new WeakMap<MediaSource, string>();
	const createUrl = URL.createObjectURL;
	URL.createObjectURL = (object) => {
		const url = createUrl(object);
		if (object instanceof MediaSource) sources.set(object, url);
		return url;
	};
	let nextBuffer = 0;
	const addBuffer = MediaSource.prototype.addSourceBuffer;
	MediaSource.prototype.addSourceBuffer = function (mime) {
		const buffer = addBuffer.call(this, mime);
		const id = ++nextBuffer;
		const source = sources.get(this) ?? '';
		const append = buffer.appendBuffer.bind(buffer);
		buffer.appendBuffer = (data) => {
			const bytes = ArrayBuffer.isView(data)
				? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
				: new Uint8Array(data);
			if (probe.appends.length < 256) {
				probe.appends.push({
					source,
					buffer: id,
					mime,
					mode: buffer.mode,
					box: String.fromCharCode(...bytes.subarray(4, 8))
				});
			} else probe.overflow = true;
			append(data);
		};
		return buffer;
	};
}

export async function installRecordedAudioMseProbe(page: Page) {
	await page.addInitScript(installProbe);
}

export async function correctedMseAppends(page: Page, source: string, initializationCount = 1) {
	const probe = await page.evaluate(() => (window as unknown as ProbeWindow).recordedAudioMseProbe);
	expect(probe.overflow).toBe(false);
	const appends = probe.appends.filter((append) => append.source === source);
	expect(appends.filter((append) => append.box === 'ftyp').length).toBeGreaterThanOrEqual(
		initializationCount
	);
	expect(new Set(appends.map((append) => append.buffer)).size).toBe(1);
	for (const append of appends) {
		expect(append.mode).toBe('segments');
		expect(append.mime).toContain('avc1.');
		expect(append.mime).toContain('mp4a.');
	}
	return appends;
}
