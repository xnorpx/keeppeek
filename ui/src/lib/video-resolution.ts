const edgeCropTolerancePixels = 16;

export function videoResolutionMatches(
	profileResolution: string | null | undefined,
	width: number,
	height: number
): boolean {
	const match = profileResolution?.match(/^(\d+)\s*[x×]\s*(\d+)$/i);
	if (!match) return true;
	const expectedWidth = Number(match[1]);
	const expectedHeight = Number(match[2]);
	return (
		Math.abs(width - expectedWidth) <= edgeCropTolerancePixels &&
		Math.abs(height - expectedHeight) <= edgeCropTolerancePixels
	);
}
