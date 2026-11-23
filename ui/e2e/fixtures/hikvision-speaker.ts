import type { ChildProcessByStdio } from 'node:child_process';
import type { Readable, Writable } from 'node:stream';

export type SpeakerObservation = {
	active: boolean;
	bytes: number;
	audible: number;
	crossings: number;
	peerClosedUploads: number;
};

export function observeSpeaker(
	child: ChildProcessByStdio<Writable, Readable, Readable>
): Promise<SpeakerObservation> {
	return new Promise((resolve, reject) => {
		let output = '';
		const finish = (error?: Error, result?: SpeakerObservation) => {
			clearTimeout(timer);
			child.stdout.off('data', onData);
			child.off('exit', onExit);
			if (result) resolve(result);
			else reject(error);
		};
		const onExit = () =>
			finish(new Error('Hikvision fixture exited before its speaker observation'));
		const onData = (chunk: Buffer) => {
			output += chunk.toString();
			if (Buffer.byteLength(output) > 160)
				return finish(new Error('Oversized speaker observation'));
			if (!output.endsWith('\n')) return;
			const match = /^speaker (true|false) (\d+) (\d+) (\d+) (\d+)$/.exec(output.trim());
			if (!match) {
				const shape = output
					.trim()
					.split(/\s+/, 8)
					.map((field) =>
						/^\d+$/.test(field) ? `${field.length} digits` : `${field.length} chars`
					);
				return finish(
					new Error(
						`Malformed speaker observation (${Buffer.byteLength(output)} bytes, fields ${shape.join(', ')})`
					)
				);
			}
			finish(undefined, {
				active: match[1] === 'true',
				bytes: Number(match[2]),
				audible: Number(match[3]),
				crossings: Number(match[4]),
				peerClosedUploads: Number(match[5])
			});
		};
		const timer = setTimeout(
			() => finish(new Error('Hikvision speaker observation timed out')),
			3_000
		);
		child.stdout.on('data', onData);
		child.once('exit', onExit);
		child.stdin.write('s', (error) => {
			if (error) finish(error);
		});
	});
}
