// The server's own pages (crates/conde-remote/src/web/pages.rs) inline the app's font faces;
// a changed lib/fonts.css without its copy there fails here.
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
