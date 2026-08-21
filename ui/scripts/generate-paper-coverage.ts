import { writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { renderPaperCoverageReport } from './paper-coverage';

const output = resolve('design/paper/keeppeek-nvr-v34/COVERAGE.md');
await writeFile(output, await renderPaperCoverageReport(), 'utf8');
console.log(`Wrote ${output}`);
