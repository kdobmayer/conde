// The web app only reads: a request carries no idempotency key, and a read refused for a stale
// session (another sign-in replaced it) reloads the session and retries once with its token.
import assert from 'node:assert/strict';
import { test } from 'node:test';

test('a stale session is reloaded and the read retried once with the new token', async () => {
	let sessionReads = 0;
	const requests = [];
	globalThis.fetch = async (url, options) => {
		if (url === '/v1/session') {
			sessionReads++;
			return Response.json({ user: 'alice', csrf: `token-${sessionReads}`, csrf_header: 'x-csrf-token' });
		}
		assert.equal(url, '/v1/rpc');
		requests.push({ request: JSON.parse(options.body), csrf: options.headers['x-csrf-token'] });
		if (requests.length === 1) {
			return Response.json({ ok: false, error: { code: 'stale_session', message: 'session replaced' } }, { status: 403 });
		}
		return Response.json({ ok: true, result: { owner: 'alice', agents: [] } });
	};
	const { rpc } = await import('../src/lib/api.ts');
	const rail = await rpc('owner.list');
	assert.equal(rail.owner, 'alice');
	assert.equal(requests.length, 2);
	assert.equal(sessionReads, 2);
	assert.deepEqual(requests.map((r) => r.csrf), ['token-1', 'token-2']);
	assert.ok(requests.every((r) => r.request.idempotency_key === undefined), 'reads carry no key');
});

test('concurrent 401s of a resumed page start one sign-in', async () => {
	const navigations = [];
	globalThis.location = { hash: '#/a/alice', set href(url) { navigations.push(url); } };
	globalThis.fetch = async () => new Response(null, { status: 401 });
	const { loadSession } = await import('../src/lib/api.ts?signin-once');
	const settled = await Promise.allSettled([loadSession(), loadSession(), loadSession()]);
	assert.ok(settled.every((s) => s.status === 'rejected' && s.reason.code === 'unauthenticated'));
	assert.deepEqual(navigations, ['/auth/login?next=' + encodeURIComponent('/#/a/alice')]);
});

test('a page restored from the back/forward cache can sign in again', async () => {
	const navigations = [];
	const listeners = {};
	globalThis.window = { addEventListener: (type, fn) => (listeners[type] = fn) };
	globalThis.location = { hash: '', set href(url) { navigations.push(url); } };
	globalThis.fetch = async () => new Response(null, { status: 401 });
	const { loadSession } = await import('../src/lib/api.ts?signin-bfcache');
	await loadSession().catch(() => {});
	listeners.pageshow({ persisted: false });
	await loadSession().catch(() => {});
	assert.equal(navigations.length, 1, 'an ordinary pageshow keeps the latch');
	listeners.pageshow({ persisted: true });
	await loadSession().catch(() => {});
	assert.equal(navigations.length, 2, 'a restored page signs in again');
	delete globalThis.window;
});
