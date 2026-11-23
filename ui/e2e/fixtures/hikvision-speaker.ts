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
	child: ChildProcessByStdio<Writable, Readable, null>
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
			if (output.length > 160) return finish(new Error('Oversized speaker observation'));
			if (!output.endsWith('\n')) return;
			const match = /^speaker (true|false) (\d+) (\d+) (\d+) (\d+)$/.exec(output.trim());
			if (!match) return finish(new Error('Malformed speaker observation'));
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
