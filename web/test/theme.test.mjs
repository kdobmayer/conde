// The theme and look choices (lib/theme.ts) and the inline script that applies them before the
// first paint, in the app shell and, as a copy, in the server's own pages.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';
import { asChoice, asUi, resolve } from '../src/lib/theme.ts';

const read = (p) => readFileSync(new URL(p, import.meta.url), 'utf8');
const appJs = read('../src/app.html').match(/<script>([^<]+)<\/script>/)?.[1];
const pagesJs = read('../../crates/conde-remote/src/web/pages.rs').match(/const THEME_JS: &str = r#"([\s\S]*?)"#;/)?.[1];

test('a stored value reads as a choice; anything else is system', () => {
	assert.deepEqual(['light', 'dark', 'system', null, '', 'Dark', 'blue'].map(asChoice), ['light', 'dark', 'system', 'system', 'system', 'system', 'system']);
});

test('a stored look reads as beta only when it says beta; anything else is current', () => {
	assert.deepEqual(['beta', 'current', null, '', 'Beta', 'jade'].map(asUi), ['beta', 'current', 'current', 'current', 'current', 'current']);
});

test('system follows the browser; light and dark override it', () => {
	assert.deepEqual(
		[resolve('system', true), resolve('system', false), resolve('light', false), resolve('dark', true)],
		['light', 'dark', 'light', 'dark']
	);
});

test('the server pages carry the same inline script as the app shell; the CSP allows it by hash', () => {
	assert.ok(appJs, 'app.html has the theme script');
	assert.equal(pagesJs, appJs);
	const hash = createHash('sha256').update(appJs).digest('base64');
	assert.ok(read('../vite.config.ts').includes(`'sha256-${hash}'`), `vite.config.ts allows sha256-${hash}`);
});

/** Runs the inline script with a stored theme and look and a system preference; returns
 * data-theme and data-ui. */
function boot(stored, systemLight, { throws = false, ui = null, dataset = {} } = {}) {
	const root = { dataset };
	const listeners = {};
	runInNewContext(appJs, {
		document: { documentElement: root },
		localStorage: {
			getItem: (k) => {
				if (throws) throw new Error('denied');
				return k === 'conde.theme' ? stored : k === 'conde.ui' ? ui : null;
			}
		},
		matchMedia: () => ({ matches: systemLight, addEventListener: (t, f) => (listeners[`media:${t}`] = f) }),
		addEventListener: (t, f) => (listeners[t] = f)
	});
	return { theme: root.dataset.theme, ui: root.dataset.ui, listeners };
}

test('the inline script applies the stored choice, else the system, and survives blocked storage', () => {
	assert.equal(boot('light', false).theme, 'light');
	assert.equal(boot('dark', true).theme, 'dark');
	assert.equal(boot(null, true).theme, 'light');
	assert.equal(boot('junk', false).theme, 'dark');
	assert.equal(boot('light', false, { throws: true }).theme, 'dark');
	const { listeners } = boot(null, false);
	assert.ok(listeners['media:change'] && listeners.storage, 'follows the system and other tabs');
});

test('the inline script marks the beta look, clears it when it goes, and survives blocked storage', () => {
	assert.equal(boot('dark', false, { ui: 'beta' }).ui, 'beta');
	assert.equal(boot(null, true, { ui: 'beta' }).theme, 'light', 'the look leaves the theme alone');
	assert.equal(boot('dark', false, { ui: 'junk' }).ui, undefined);
	assert.equal(boot('dark', false, { ui: null, dataset: { ui: 'beta' } }).ui, undefined, 'another tab went back to current');
	assert.equal(boot('dark', false, { ui: 'beta', throws: true }).ui, undefined);
});
