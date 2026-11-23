import type { ReportGrant, ReportState, ReportTransport } from './report-submission';

const maxStatusBytes = 8 * 1024;

function reportUrl(service: URL, grant: ReportGrant, suffix: string): URL {
	if (!/^[A-Za-z0-9_-]{1,128}$/.test(grant.id)) throw new Error('Invalid report identifier.');
	return new URL(`v1/reports/${grant.id}${suffix}`, service);
}

async function reportStatus(response: Response): Promise<ReportState> {
	const reader = response.body?.getReader();
	if (!reader) throw new Error('Missing report status.');
	const decoder = new TextDecoder('utf-8', { fatal: true });
	let json = '';
	let bytes = 0;
	try {
		while (true) {
			const { value, done } = await reader.read();
			if (done) break;
			bytes += value.byteLength;
			if (bytes > maxStatusBytes) throw new Error('Report status was too large.');
			json += decoder.decode(value, { stream: true });
		}
		json += decoder.decode();
	} finally {
		await reader.cancel().catch(() => undefined);
		reader.releaseLock();
	}
	const payload: unknown = JSON.parse(json);
	if (!payload || typeof payload !== 'object' || !('state' in payload)) {
		throw new Error('Invalid report status.');
	}
	switch (payload.state) {
		case 'pending':
		case 'failed':
		case 'cancelled':
			return { state: payload.state };
		case 'complete':
			if ('issueUrl' in payload && typeof payload.issueUrl === 'string') {
				return { state: 'complete', issueUrl: payload.issueUrl };
			}
	}
	throw new Error('Invalid report status.');
}

/** Grant creation is always supplied by the authenticated recorder, never by this service client. */
export function createRemoteReportTransport(
	serviceOrigin: string,
	create: ReportTransport['create'],
	request: typeof fetch = fetch
): ReportTransport {
	const service = new URL(serviceOrigin);
	if (
		service.protocol !== 'https:' ||
		service.username ||
		service.password ||
		service.pathname !== '/' ||
		service.search ||
		service.hash
	) {
		throw new Error('Reporting requires a configured HTTPS service origin.');
	}

	async function send(
		grant: ReportGrant,
		suffix: string,
		signal: AbortSignal,
		method: 'GET' | 'POST' | 'PUT' | 'DELETE',
		artifact?: Blob
	): Promise<Response> {
		const response = await request(reportUrl(service, grant, suffix), {
			method,
			signal,
			body: artifact,
			credentials: 'omit',
			redirect: 'error',
			cache: 'no-store',
			referrerPolicy: 'no-referrer',
			headers: {
				Authorization: `Bearer ${grant.token}`,
				...(artifact ? { 'Content-Type': artifact.type } : { Accept: 'application/json' })
			}
		});
		if (!response.ok) {
			await response.body?.cancel().catch(() => undefined);
			throw new Error('The reporting service did not accept the request.');
		}
		return response;
	}

	return {
		create,
		async status(grant, signal) {
			return reportStatus(await send(grant, '', signal, 'GET'));
		},
		async upload(grant, kind, artifact, signal, onBytes) {
			const response = await send(grant, `/artifacts/${kind}`, signal, 'PUT', artifact);
			await response.body?.cancel().catch(() => undefined);
			onBytes(artifact.size);
		},
		async finalize(grant, signal) {
			return reportStatus(await send(grant, '/finalize', signal, 'POST'));
		},
		async cancel(grant, signal) {
			const response = await send(grant, '', signal, 'DELETE');
			await response.body?.cancel().catch(() => undefined);
		}
	};
}
