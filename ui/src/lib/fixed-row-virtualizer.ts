export type FixedRowWindow = {
	startIndex: number;
	endIndex: number;
	offsetTop: number;
	totalHeight: number;
};

type FixedRowOptions = {
	rowHeight: number;
	overscan: number;
	maxItems: number;
};

export function fixedRowWindow(
	itemCount: number,
	scrollTop: number,
	viewportHeight: number,
	options: FixedRowOptions
): FixedRowWindow {
	const count = Math.max(0, Math.floor(itemCount));
	if (options.rowHeight <= 0) throw new Error('Virtual row height must be positive');
	if (options.overscan < 0) throw new Error('Virtual overscan cannot be negative');
	if (options.maxItems <= 0) throw new Error('Virtual item limit must be positive');

	const totalHeight = count * options.rowHeight;
	if (count === 0) return { startIndex: 0, endIndex: 0, offsetTop: 0, totalHeight };

	const firstVisible = Math.floor(Math.max(0, scrollTop) / options.rowHeight);
	const visibleCount = Math.max(1, Math.ceil(Math.max(0, viewportHeight) / options.rowHeight));
	const windowSize = Math.min(count, options.maxItems, visibleCount + options.overscan * 2);
	const unclampedStart = Math.max(0, firstVisible - options.overscan);
	const startIndex = Math.min(unclampedStart, count - windowSize);
	const endIndex = Math.min(count, startIndex + windowSize);

	return {
		startIndex,
		endIndex,
		offsetTop: startIndex * options.rowHeight,
		totalHeight
	};
}
