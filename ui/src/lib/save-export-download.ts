import type { MediaExportDownload } from './control-client';

export function saveExportDownload(download: MediaExportDownload): void {
	const url = URL.createObjectURL(download.blob);
	const anchor = document.createElement('a');
	anchor.href = url;
	anchor.download = download.job.fileName ?? 'keeppeek-export.mp4';
	document.body.append(anchor);
	anchor.click();
	anchor.remove();
	setTimeout(() => URL.revokeObjectURL(url), 0);
}
