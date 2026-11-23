// Independent evidence sources may stall even while the authenticated control session is healthy.
export async function boundedReportEvidence<T>(
	owner: AbortSignal,
	operation: (signal: AbortSignal) => Promise<T>,
	timeoutMs = 4_000
): Promise<T> {
	owner.throwIfAborted();
	const controller = new AbortController();
	const onOwnerAbort = () => controller.abort(owner.reason);
	owner.addEventListener('abort', onOwnerAbort, { once: true });
	const timer = setTimeout(
		() => controller.abort(new DOMException('Evidence deadline exceeded', 'TimeoutError')),
		timeoutMs
	);
	let rejectCancellation!: (reason: unknown) => void;
	const cancellation = new Promise<never>((_resolve, reject) => {
		rejectCancellation = reject;
	});
	const abort = () => rejectCancellation(controller.signal.reason);
	controller.signal.addEventListener('abort', abort, { once: true });
	try {
		return await Promise.race([cancellation, operation(controller.signal)]);
	} finally {
		clearTimeout(timer);
		owner.removeEventListener('abort', onOwnerAbort);
		controller.signal.removeEventListener('abort', abort);
	}
}
