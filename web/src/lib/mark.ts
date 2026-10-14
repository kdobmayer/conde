/**
 * The conde mark, the one source of the header mark (Mark.svelte, and mark.svg for the server's
 * own pages) and the favicons; `npm run icons` writes static/app/ from it. Two filled shapes on
 * a 16-unit grid: `back` in the muted tone, `front` in the text tone.
 */
export const mark = { back: 'M8 3h6v6H8Z', front: 'M2 2h3v9h9v3H2Z' };

/** --fg and --muted of app.css, which a favicon cannot read. */
export const palette = {
	light: { front: '#1c1917', back: '#78716c' },
	dark: { front: '#f5f5f4', back: '#a8a29e' }
};

const open = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16"';

/** The favicon: the light palette, or the dark one when the browser is dark. */
export function faviconSvg(): string {
	const { light, dark } = palette;
	return (
		`${open}><style>.b{fill:${light.back}}.f{fill:${light.front}}` +
		`@media (prefers-color-scheme:dark){.b{fill:${dark.back}}.f{fill:${dark.front}}}</style>` +
		`<path class="b" d="${mark.back}"/><path class="f" d="${mark.front}"/></svg>\n`
	);
}

/** One fixed palette, for the PNG fallbacks. */
export function flatSvg(theme: keyof typeof palette): string {
	const c = palette[theme];
	return `${open}><path fill="${c.back}" d="${mark.back}"/><path fill="${c.front}" d="${mark.front}"/></svg>\n`;
}

/** The mark inline in a page with the app's tokens, as Mark.svelte draws it. */
export function inlineSvg(): string {
	return (
		`${open} class="mark" aria-hidden="true" focusable="false">` +
		`<path style="fill:var(--muted)" d="${mark.back}"/><path fill="currentColor" d="${mark.front}"/></svg>\n`
	);
}
