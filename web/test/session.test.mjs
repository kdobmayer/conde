// Another sign-in (a different owner) in a second tab: this tab never shows the new owner's
// data under the old owner's routes; it starts over (review repro owner-session-switch, inverted).
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { test } from 'node:test';

test('a stale session that belongs to another owner restarts the app instead of mixing owners', async () => {
	const api = await import('../src/lib/api.ts');
	let owner = 'alice';
	globalThis.fetch = async (url, options) => {
		if (url === '/v1/session') return Response.json({ user: owner, display: owner, csrf: owner + '-token', csrf_header: 'x-csrf-token' });
		if (options.headers['x-csrf-token'] !== owner + '-token') {
			return Response.json({ ok: false, error: { code: 'stale_session', message: 'session replaced' } }, { status: 403 });
		}
		return Response.json({ ok: true, result: { owner, agents: [{ name: owner + '/worker', dms: [{ with: 'david/scout' }], channels: [] }] } });
	};
	const replaced = [];
	globalThis.location = { replace: (url) => replaced.push(url), protocol: 'http:', host: 'x', hash: '' };
	const source = readFileSync(new URL('../src/lib/board.svelte.ts', import.meta.url), 'utf8')
		.replace(/^import .*;$/gm, '')
		.replace(/^export /gm, '');
	const b = new Function('$state', 'loadSession', 'rpc', 'signIn', 'onSession', 'currentSession',
		stripTypeScriptTypes(source) + '\nreturn {board, refreshRail, threadHref};')(
		(x) => x, api.loadSession, api.rpc, () => {}, api.onSession, api.currentSession);
	await api.loadSession();
	await b.refreshRail();
	assert.equal(b.board.session.user, 'alice');
	assert.equal(b.board.rail.owner, 'alice');

	owner = 'bob';
	await b.refreshRail();
	assert.deepEqual(replaced, ['/'], 'the app starts over for the new owner');
	assert.equal(b.board.session.user, 'alice', 'the old owner stays on screen until it does');
	assert.equal(b.board.rail.owner, 'alice', "bob's rail never lands under alice's routes");
	assert.equal(api.currentSession().user, 'bob');

	// The same owner's new token just replaces the session.
	replaced.length = 0;
	owner = 'alice';
	await api.loadSession();
	assert.deepEqual(replaced, [], 'no restart for the same owner');
	assert.equal(b.board.session.csrf, 'alice-token');
});
