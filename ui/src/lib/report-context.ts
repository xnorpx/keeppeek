import { getContext, setContext } from 'svelte';
import type { ReportSession } from './report-session.svelte';

const contextKey = Symbol('keeppeek.report-session');
type ReportContext = { session: ReportSession; show(): void; authorized(): boolean };

export function provideReportContext(context: ReportContext): void {
	setContext(contextKey, context);
}

export function useReportContext(): ReportContext | undefined {
	return getContext(contextKey);
}
