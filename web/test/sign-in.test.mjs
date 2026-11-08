// A signed-out `/` is the server's sign-in page (crates/conde-remote/src/web/app.rs): its inline
// script signs in with the view in the URL fragment, which the server never sees, the same way
// the app's own sign-in (lib/api.ts) does.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const appRs = readFileSync(new URL('../../crates/conde-remote/src/web/app.rs', import.meta.url), 'utf8');
const signInJs = appRs.match(/const SIGN_IN_JS: &str =\s*r#"([\s\S]*?)"#;/)?.[1];

/** Where the signed-out page sends a browser that opened `/` with `hash`. */
function bounce(hash) {
	const went = [];
	runInNewContext(signInJs, { location: { hash, replace: (url) => went.push(url) }, encodeURIComponent });
	return went;
}

test('the signed-out page signs in and returns to the view in the fragment', () => {
	assert.ok(signInJs, 'app.rs has the sign-in script');
	assert.deepEqual(bounce('#/machine'), ['/auth/login?next=' + encodeURIComponent('/#/machine')]);
	assert.deepEqual(bounce('#/a/alice/dm/bob/scout'), ['/auth/login?next=%2F%23%2Fa%2Falice%2Fdm%2Fbob%2Fscout']);
	assert.deepEqual(bounce(''), ['/auth/login?next=%2F']);
});

test('the signed-out page and the app send the same sign-in for a view', async () => {
	const navigations = [];
	globalThis.location = { hash: '#/fleet', set href(url) { navigations.push(url); } };
	const { signIn } = await import('../src/lib/api.ts?sign-in-page');
	signIn();
	assert.deepEqual(bounce('#/fleet'), navigations);
	delete globalThis.location;
});
