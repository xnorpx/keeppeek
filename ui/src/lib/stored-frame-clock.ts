// Sequence-mode MSE reports video timestamps after shifting all tracks by the same offset.
// The catalog indexes fragments by their first video decode timestamp in whole milliseconds.
type Box = { type: string; data: DataView };
function boxes(parent: DataView): Box[] {
	const found: Box[] = [];
	for (let offset = 0; offset < parent.byteLength;) {
		if (parent.byteLength - offset < 8 || found.length >= 256) throw new Error('Invalid MP4 boxes');
		const declared = parent.getUint32(offset);
		const header = declared === 1 ? 16 : 8;
		const size =
			declared === 1
				? Number(parent.getBigUint64(offset + 8))
				: declared || parent.byteLength - offset;
		if (!Number.isSafeInteger(size) || size < header || size > parent.byteLength - offset)
			throw new Error('Invalid MP4 box size');
		const type = String.fromCharCode(
			...new Uint8Array(parent.buffer, parent.byteOffset + offset + 4, 4)
		);
		found.push({
			type,
			data: new DataView(parent.buffer, parent.byteOffset + offset + header, size - header)
		});
		offset += size;
	}
	return found;
}

function child(nodes: Box[], type: string): DataView {
	const node = nodes.find((node) => node.type === type);
	if (!node) throw new Error('Missing MP4 box');
	return node.data;
}

type Track = { id: number; rate: bigint };
function videoTrack(input: DataView): Track | undefined {
	const tracks = boxes(child(boxes(input), 'moov')).filter((node) => node.type === 'trak');
	for (const track of tracks) {
		const parts = boxes(track.data);
		const media = boxes(child(parts, 'mdia'));
		if (child(media, 'hdlr').getUint32(8) !== 0x76696465) continue; // vide
		const header = child(parts, 'tkhd');
		const clock = child(media, 'mdhd');
		if (header.getUint8(0) > 1 || clock.getUint8(0) > 1) return;
		const id = header.getUint32(header.getUint8(0) === 1 ? 20 : 12);
		const rate = BigInt(clock.getUint32(clock.getUint8(0) === 1 ? 20 : 12));
		if (id && rate) return { id, rate };
	}
}

function firstVideoTime(input: DataView, track: Track) {
	for (const node of boxes(child(boxes(input), 'moof'))) {
		if (node.type !== 'traf') continue;
		const parts = boxes(node.data);
		if (child(parts, 'tfhd').getUint32(4) !== track.id) continue;
		const tfdt = child(parts, 'tfdt');
		const run = child(parts, 'trun');
		if (tfdt.getUint8(0) > 1 || run.getUint8(0) > 1) return;
		const dts = tfdt.getUint8(0) === 1 ? tfdt.getBigUint64(4) : BigInt(tfdt.getUint32(4));
		if (run.getUint32(4) === 0) return;
		const flags = run.getUint32(0) & 0xffffff;
		let composition = 0;
		if (flags & 0x800) {
			let offset = 8;
			for (const field of [1, 4, 0x100, 0x200, 0x400]) if (flags & field) offset += 4;
			composition = run.getUint8(0) === 1 ? run.getInt32(offset) : run.getUint32(offset);
		}
		return { dts, pts: dts + BigInt(composition) };
	}
}

type Period = { mediaStartUs: number; originNs: bigint };

export class StoredFrameClock {
	private track?: Track;
	private periods: Period[] = [];

	reset() {
		this.track = undefined;
		this.periods = [];
	}

	initialization(bytes: Uint8Array) {
		try {
			this.track = videoTrack(new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength));
		} catch {
			this.track = undefined;
		}
	}

	fragment(bytes: Uint8Array, catalogStartMs: number, timestampOffset: number) {
		try {
			if (!this.track || !Number.isSafeInteger(catalogStartMs))
				throw new Error('Missing video clock');
			const times = firstVideoTime(
				new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength),
				this.track
			);
			const offsetUs = Math.round(timestampOffset * 1_000_000);
			if (!times || !Number.isSafeInteger(offsetUs)) throw new Error('Missing fragment clock');
			const { rate } = this.track;
			const recordingMs = BigInt(catalogStartMs) - (times.dts * 1_000n) / rate;
			const originNs = recordingMs * 1_000_000n - BigInt(offsetUs) * 1_000n;
			if (this.periods.at(-1)?.originNs === originNs) return;
			const mediaStartUs = Number((times.pts * 1_000_000n) / rate) + offsetUs;
			if (!Number.isSafeInteger(mediaStartUs)) throw new Error('Invalid video clock');
			// A generation is normally one continuous period. Bound discontinuities in long sessions.
			this.periods = this.periods
				.filter((period) => period.mediaStartUs < mediaStartUs)
				.slice(-255);
			this.periods.push({ mediaStartUs, originNs });
		} catch {
			this.periods = [];
		}
	}

	storedNs(mediaUs: number): bigint | undefined {
		const period = this.periods.findLast((period) => period.mediaStartUs <= mediaUs);
		return period && period.originNs + BigInt(mediaUs) * 1_000n;
	}

	mediaUs(storedNs: bigint): number | undefined {
		let matching: number | undefined;
		for (const [index, period] of this.periods.entries()) {
			const us = Math.round(Number(storedNs - period.originNs) / 1_000);
			if (
				Number.isSafeInteger(us) &&
				us >= period.mediaStartUs &&
				us < (this.periods[index + 1]?.mediaStartUs ?? Infinity)
			) {
				if (matching !== undefined) return;
				matching = us;
			}
		}
		return matching;
	}
}
