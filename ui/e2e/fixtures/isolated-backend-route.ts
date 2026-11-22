import type { Route } from '@playwright/test';

export function isolatedBackendRoute(origin: string, backend: string) {
	return async (route: Route) => {
		const url = new URL(route.request().url());
		if (
			url.origin === origin &&
			(url.pathname === '/create' ||
				url.pathname === '/delete' ||
				url.pathname.startsWith('/api/') ||
				url.pathname.startsWith('/auth/') ||
				url.pathname.startsWith('/config/') ||
				url.pathname === '/metrics' ||
				url.pathname === '/logs')
		) {
			const response = await route.fetch({ url: `${backend}${url.pathname}${url.search}` });
			await route.fulfill({ response });
		} else await route.continue();
	};
}
