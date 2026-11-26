import { expect, test } from '@playwright/test';
import { keepModeDate, keepModeDayStartMs, mockKeepModes } from './fixtures/keep-modes';

const timelapseCaps = [
	'keeppeek.media-export.v1',
	'keeppeek.timelapse.v1',
	'keeppeek.timelapse-export.v1'
];

test('Timelapse review shows presets, explicit gaps, preview, partial policy, seek handoff, and silent MP4 + manifest download', async ({
	page
}) => {
	test.setTimeout(90_000);
	const sampleOneMs = keepModeDayStartMs + 6 * 60 * 60_000 + 20 * 60_000;
	const sampleTwoMs = sampleOneMs + 15 * 60_000;
	const gapStartMs = sampleOneMs + 5 * 60_000;
	const gapEndMs = sampleOneMs + 10 * 60_000;

	const controls = await mockKeepModes(page, 2, {
		capabilityIds: timelapseCaps,
		timelapsePlan: () => ({
			samples: [
				{
					index: 0,
					requestedMs: sampleOneMs,
					actualMs: sampleOneMs,
					codec: 'h264',
					width: 640,
					height: 360
				},
				{
					index: 1,
					requestedMs: sampleTwoMs,
					actualMs: sampleTwoMs,
					codec: 'h265',
					width: 1280,
					height: 720
				}
			],
			gaps: [{ startMs: gapStartMs, endMs: gapEndMs, reason: 'privacy' }],
			requiresConfirmation: true,
			encoderAvailable: true,
			siteTimezone: 'America/Los_Angeles'
		}),
		exportCreateResults: [{ status: 'running', progress: 0.4 }],
		exportGetResults: [
			{
				status: 'ready',
				progress: 1,
				fileName: 'front-door-timelapse.mp4',
				bytesWritten: 2048
			}
		]
	});

	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=timelapse`);
	await expect(page.locator('[data-timelapse-review]')).toBeVisible({ timeout: 45_000 });

	// Preview summary shows explicit sample count, duration, gaps, and site timezone.
	await expect(page.locator('[data-timelapse-estimate-samples]')).toHaveText('2');
	await expect(page.locator('[data-timelapse-estimate-gaps]')).toContainText('1');
	await expect(page.locator('[data-timelapse-preview-summary]')).toContainText('Silent');
	await expect(page.locator('[data-timelapse-preview-summary]')).toContainText(
		'America/Los_Angeles'
	);

	// Gap card is explicitly rendered with privacy reason and never disguised by a stale frame.
	await expect(page.locator('[data-timelapse-gap-card][data-gap-reason="privacy"]')).toBeVisible();
	await expect(page.locator('[data-timelapse-sample-card]')).toHaveCount(2);

	// Presets switch cleanly including custom range controls.
	await page.locator('[data-timelapse-preset="last_15m"]').click();
	await page.locator('[data-timelapse-preset="custom"]').click();
	await expect(page.locator('[data-timelapse-custom-start]')).toBeVisible();
	await expect(page.locator('[data-timelapse-custom-end]')).toBeVisible();
	await page.locator('[data-timelapse-preset="last_hour"]').click();

	// Editing settings invalidates confirmation and disables Generate until Preview plan refreshes.
	await page.locator('[data-timelapse-allow-partial]').check();
	await page.locator('[data-timelapse-confirm-expensive]').check();
	const generateBtn = page.locator('[data-timelapse-generate]');
	await expect(generateBtn).toBeEnabled();
	await page.locator('[data-timelapse-overlay]').uncheck();
	await expect(page.locator('[data-timelapse-stale-preview]')).toBeVisible();
	await expect(generateBtn).toBeDisabled();
	await page.locator('[data-timelapse-plan-refresh]').click();
	await expect(page.locator('[data-timelapse-stale-preview]')).toHaveCount(0);

	// Generate button stays disabled until BOTH partial allowance and expensive confirmation are checked.
	await expect(generateBtn).toBeDisabled();
	await page.locator('[data-timelapse-confirm-expensive]').check();
	await expect(generateBtn).toBeEnabled();

	// Submit silent timelapse MP4 job and watch it progress to Ready with MP4 & Manifest buttons.
	await generateBtn.click();
	await expect(page.locator('[data-timelapse-active-job]')).toHaveAttribute(
		'data-job-status',
		'ready',
		{
			timeout: 15_000
		}
	);
	await expect(page.locator('[data-timelapse-download-mp4]')).toBeVisible();
	await expect(page.locator('[data-timelapse-download-manifest]')).toBeVisible();

	// Verify download requests trigger for both MP4 and Manifest JSON.
	const mp4Download = page.waitForEvent('download');
	await page.locator('[data-timelapse-download-mp4]').click();
	const mp4File = await mp4Download;
	expect(mp4File.suggestedFilename()).toContain('.mp4');

	const manifestDownload = page.waitForEvent('download');
	await page.locator('[data-timelapse-download-manifest]').click();
	const manifestFile = await manifestDownload;
	expect(manifestFile.suggestedFilename()).toContain('.manifest.json');

	// Switching cameras clears the previous camera's activeJob and refreshes cleanly.
	await page.getByRole('button', { name: 'Next camera, Camera 2' }).click();
	await expect(page).toHaveURL(/camera=camera-2/);
	await expect(page.locator('[data-timelapse-active-job]')).toHaveCount(0);
	await page.getByRole('button', { name: 'Previous camera, Front Door' }).click();
	await expect(page).toHaveURL(/camera=front-door/);

	// Clicking a sampled frame seeks normal Keep Timeline to its exact source timestamp.
	await page.locator('[data-timelapse-sample-card]').first().click();
	await expect(page.getByRole('button', { name: 'Timeline', exact: true })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-playhead-ms',
		String(sampleOneMs)
	);
	await expect
		.poll(
			() =>
				controls.storedOpens.some((open) => open.timestampMs === sampleOneMs) ||
				controls.storedSeeks.some((seek) => seek.timestampMs === sampleOneMs)
		)
		.toBe(true);
});

test('Timelapse layout stays accessible and usable on mobile viewport', async ({ page }) => {
	test.setTimeout(60_000);
	await page.setViewportSize({ width: 390, height: 844 });
	await mockKeepModes(page, 1, {
		capabilityIds: timelapseCaps,
		timelapsePlan: () => ({
			samples: [
				{
					index: 0,
					requestedMs: keepModeDayStartMs + 6 * 60 * 60_000,
					actualMs: keepModeDayStartMs + 6 * 60 * 60_000
				}
			],
			gaps: []
		})
	});

	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=timelapse`);
	await expect(page.locator('[data-timelapse-review]')).toBeVisible({ timeout: 45_000 });
	await expect(page.locator('[data-timelapse-preset="last_hour"]')).toBeVisible();
	await expect(page.locator('[data-timelapse-export-panel]')).toBeVisible();
});
