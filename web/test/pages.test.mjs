// The server's own pages (crates/conde-remote/src/web/pages.rs) inline the app's font faces and
// the beta look's colours; a changed lib/fonts.css or beta block without its copy there fails here.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const read = (p) => readFileSync(new URL(p, import.meta.url), 'utf8');
/** The @font-face rules of a stylesheet, whitespace and comments removed. */
const faces = (css) => [...css.replace(/\/\*[\s\S]*?\*\//g, '').matchAll(/@font-face\s*\{([^}]*)\}/g)].map((m) => m[1].replace(/\s+/g, '').replace(/;$/, ''));

test('the server pages carry the app’s font faces', () => {
	const app = faces(read('../src/lib/fonts.css'));
	const pages = faces(read('../../crates/conde-remote/src/web/pages.rs').match(/const CSS: &str = r#"([\s\S]*?)"#;/)[1]);
	assert.equal(app.length, 10);
	assert.deepEqual(pages, app);
});

/** The declarations of the first rule whose selector is `sel` (quotes and spaces ignored). */
function rule(css, sel) {
	const flat = css.replace(/\/\*[\s\S]*?\*\//g, '').replace(/['"]/g, '');
	const norm = (x) => x.replace(/\s+/g, '');
	for (const m of flat.matchAll(/([^{}]+)\{([^{}]*)\}/g))
		if (norm(m[1]) === norm(sel)) return Object.fromEntries(m[2].split(';').map((d) => d.split(':').map((x) => x.trim())).filter(([k, v]) => k && v));
	return null;
}

test('the server pages carry the beta look’s colours and rail as the app has them', () => {
	const app = read('../src/app.css');
	const pages = read('../../crates/conde-remote/src/web/pages.rs').match(/const CSS: &str = r#"([\s\S]*?)"#;/)[1];
	const root = ':root[data-ui=beta][data-theme=dark]';
	const appTokens = rule(app, root);
	const pageTokens = rule(pages, root);
	assert.ok(appTokens && pageTokens, 'both have the beta block');
	// the server pages draw with fewer tokens; each one they have matches the app's
	const used = Object.keys(rule(pages, ':root')).filter((k) => k.startsWith('--') && k !== '--serif' && k !== '--sans');
	assert.deepEqual(pageTokens, Object.fromEntries(used.map((k) => [k, appTokens[k]])));
	const rail = `${root} :is(.mast,.mbar,.tabs)`;
	assert.deepEqual(rule(pages, rail), rule(app, rail));
	for (const sel of [`${root} .mast .word .mark`, `${root} .mast .sections a.on`]) assert.deepEqual(rule(pages, sel), rule(app, sel), sel);
});
