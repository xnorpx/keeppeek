import type { Page } from '@playwright/test';
import { sanitizedLog } from './container';

export async function observeBrowserErrors(page: Page) {
	const errors: string[] = [];
	const onboardingNotices: string[] = [];
	let phase = 'onboarding';
	const record = ({ detail, onboardingClose }: { detail: string; onboardingClose: boolean }) => {
		const destination = phase === 'onboarding' && onboardingClose ? onboardingNotices : errors;
		if (destination.length < 20) destination.push(`${phase}: ${sanitizedLog(detail)}`);
	};
	await page.exposeFunction('reportHomeAssistantRejection', record);
	await page.addInitScript(() => {
		const report = (detail: string, onboardingClose = false) =>
			(
				window as unknown as {
					reportHomeAssistantRejection: (value: {
						detail: string;
						onboardingClose: boolean;
					}) => Promise<void>;
				}
			).reportHomeAssistantRejection({ detail, onboardingClose });
		window.addEventListener('error', (event) => {
			void report(event.message);
		});
		window.addEventListener('unhandledrejection', (event) => {
			const reason = event.reason;
			const detail = JSON.stringify({
				kind: typeof reason,
				constructor: reason?.constructor?.name,
				keys: reason && typeof reason === 'object' ? Object.keys(reason).slice(0, 10) : [],
				type: String(reason?.type ?? ''),
				name: String(reason?.name ?? ''),
				code: String(reason?.code ?? reason?.error?.code ?? ''),
				message: String(reason?.message ?? reason?.error?.message ?? '').slice(0, 512),
				target: reason?.target?.constructor?.name
			});
			const onboardingClose =
				location.pathname === '/onboarding.html' &&
				reason?.type === 'result' &&
				reason?.success === false &&
				reason?.error?.code === 3 &&
				reason?.error?.message === 'Connection lost';
			void report(detail, onboardingClose);
		});
	});
	return {
		errors,
		onboardingNotices,
		phase: (next: string) => {
			phase = next;
		}
	};
}
