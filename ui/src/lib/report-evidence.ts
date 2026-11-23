// Independent evidence sources may stall even while the authenticated control session is healthy.
export async function boundedReportEvidence<T>(
	owner: AbortSignal,
	operation: (signal: AbortSignal) => Promise<T>,
	timeoutMs = 4_000
): Promise<T> {
	owner.throwIfAborted();
	const timeout = new AbortController();
	const signal = AbortSignal.any([owner, timeout.signal]);
	const timer = setTimeout(
		() => timeout.abort(new DOMException('Evidence deadline exceeded', 'TimeoutError')),
		timeoutMs
	);
	const cancelled = Promise.withResolvers<never>();
	const abort = () => cancelled.reject(signal.reason);
	signal.addEventListener('abort', abort, { once: true });
	try {
		return await Promise.race([cancelled.promise, operation(signal)]);
	} finally {
		clearTimeout(timer);
		signal.removeEventListener('abort', abort);
	}
}
