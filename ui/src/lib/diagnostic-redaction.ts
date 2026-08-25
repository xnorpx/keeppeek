const REDACTED = '[REDACTED]';
const sensitiveKey =
	/password|passwd|secret|token|authorization|credential|api[_-]?key|cookie|access[_-]?key|private[_-]?key|session[_-]?id/i;
const privateKey =
	/(^|_)(ip|host|hostname|host_name|serial_number|hardware_id|mac_address|mount_point|working_directory|executable|path|file|directory|url|uri|origin|uid)($|_)/i;

export interface DiagnosticPrivateValue {
	value: string | null | undefined;
	replacement: string;
}

export interface DiagnosticRedactor {
	text: (value: string) => string;
	value: (value: unknown) => unknown;
}

export function createDiagnosticRedactor(
	privateValues: readonly DiagnosticPrivateValue[] = []
): DiagnosticRedactor {
	const replacements = normalizedReplacements(privateValues);

	function redactText(value: string): string {
		let redacted = value;
		for (const replacement of replacements) {
			redacted = replacePrivateValue(redacted, replacement.value, replacement.replacement);
		}
		return redacted
			.replace(/\b([a-z][a-z0-9+.-]*:\/\/)(?:[^@\s/]+@)?[^/\s?#]+/gi, '$1[REDACTED_HOST]')
			.replace(
				/\b(password|passwd|secret|token|authorization|credential|api[_-]?key|cookie|access[_-]?key|private[_-]?key|session[_-]?id)\s*[:=]\s*(?:"[^"]*"|'[^']*'|[^\s,;&]+)/gi,
				'$1=[REDACTED]'
			)
			.replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/-]+=*/gi, '$1 [REDACTED]')
			.replace(/\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b/g, REDACTED)
			.replace(
				/\b[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\b/gi,
				'[REDACTED_UUID]'
			)
			.replace(/\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b/gi, '[REDACTED_EMAIL]')
			.replace(/\b(?:[0-9a-f]{2}[:-]){5}[0-9a-f]{2}\b/gi, '[REDACTED_MAC]')
			.replace(/\b(?:\d{1,3}\.){3}\d{1,3}\b/g, '[REDACTED_IP]')
			.replace(/\b(?:[0-9a-f]{1,4}:){2,7}[0-9a-f]{0,4}\b/gi, '[REDACTED_IP]')
			.replace(/\b[a-z0-9][a-z0-9.-]*\.local\b/gi, '[REDACTED_HOST]')
			.replace(/\/(?:Users|home)\/[^/\s"'\\]+/g, '/[REDACTED_HOME]')
			.replace(/[A-Za-z]:\\Users\\[^\\\s"'/]+/g, '[REDACTED_HOME]');
	}

	function redactValue(value: unknown, key = '', seen = new WeakSet<object>()): unknown {
		if (sensitiveKey.test(key)) return REDACTED;
		if (typeof value === 'string') {
			const exactReplacement = replacements.find((candidate) => candidate.value === value);
			if (exactReplacement) return exactReplacement.replacement;
			if (privateKey.test(key)) return privateReplacement(key);
			return redactText(value);
		}
		if (!value || typeof value !== 'object') return value;
		if (seen.has(value)) return '[Circular]';
		seen.add(value);
		if (Array.isArray(value)) return value.map((item) => redactValue(item, '', seen));
		return Object.fromEntries(
			Object.entries(value).map(([name, nested]) => [name, redactValue(nested, name, seen)])
		);
	}

	return {
		text: redactText,
		value: (value) => redactValue(value)
	};
}

function normalizedReplacements(
	values: readonly DiagnosticPrivateValue[]
): Array<{ value: string; replacement: string }> {
	const replacements = new Map<string, string>();
	for (const candidate of values) {
		if (!candidate.value) continue;
		const value = candidate.value.trim();
		if (!value || value === candidate.replacement) continue;
		replacements.set(value, candidate.replacement);
	}
	return [...replacements.entries()]
		.map(([value, replacement]) => ({ value, replacement }))
		.sort((left, right) => right.value.length - left.value.length);
}

function replacePrivateValue(value: string, privateValue: string, replacement: string): string {
	if (value === privateValue) return replacement;
	if (privateValue.length < 3) return value;
	return value.split(privateValue).join(replacement);
}

function privateReplacement(key: string): string {
	if (/path|mount_point|working_directory|executable|file|directory/i.test(key)) {
		return '[REDACTED_PATH]';
	}
	if (/serial|hardware|mac|uid/i.test(key)) return '[REDACTED_IDENTIFIER]';
	return '[REDACTED_ADDRESS]';
}
