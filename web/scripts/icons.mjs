// Writes static/app/ from src/lib/mark.ts: the theme-aware favicon SVG, its 32 px PNG fallbacks
// (light and dark) and mark.svg, which the server inlines in its own pages' header.
// Needs rsvg-convert (librsvg) for the PNGs; run after changing the mark.
import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { faviconSvg, flatSvg, inlineSvg } from '../src/lib/mark.ts';

const out = new URL('../static/app/', import.meta.url);
writeFileSync(new URL('favicon.svg', out), faviconSvg());
writeFileSync(new URL('mark.svg', out), inlineSvg());
for (const theme of ['light', 'dark']) {
	const png = execFileSync('rsvg-convert', ['-w', '32', '-h', '32', '-f', 'png'], { input: flatSvg(theme) });
	writeFileSync(new URL(`favicon-${theme}-32.png`, out), png);
}
