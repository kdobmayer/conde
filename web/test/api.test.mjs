// A mutation keeps one idempotency key through a dropped response and a session reload:
// the board commits it once (review repro web-session-retry, inverted).
import assert from 'node:assert/strict';
import { test } from 'node:test';

test('a send whose response was lost is committed once across a session reload', async () => {
	let sessionReads = 0;
	let sends = 0;
	const committed = new Map();
	const requests = [];
	globalThis.fetch = async (url, options) => {
		if (url === '/v1/session') {
			sessionReads++;
			return Response.json({ user: 'alice', name: 'alice/alice', csrf: `token-${sessionReads}`, csrf_header: 'x-csrf-token' });
		}
		assert.equal(url, '/v1/rpc');
		const request = JSON.parse(options.body);
		requests.push({ request, csrf: options.headers['x-csrf-token'] });
		sends++;
		if (sends === 1) {
			committed.set(request.idempotency_key, request.body);
			throw new TypeError('Response lost after server commit');
		}
		if (sends === 2) {
			return Response.json({ ok: false, error: { code: 'stale_session', message: 'session replaced' } }, { status: 403 });
		}
		committed.set(request.idempotency_key, request.body);
		return Response.json({ ok: true, result: { message_id: 'accepted' } });
	};
	const { rpc } = await import('../src/lib/api.ts');
	const result = await rpc('message.send', { to: { agent: 'bob/worker' }, text: 'run task' });
	assert.equal(result.message_id, 'accepted');
	assert.equal(requests.length, 3);
	const keys = new Set(requests.map((r) => r.request.idempotency_key));
	assert.equal(keys.size, 1, 'one key for the whole send');
	assert.equal(committed.size, 1, 'committed once');
	assert.equal(sessionReads, 2, 'the session was reloaded once');
	assert.equal(requests[2].csrf, 'token-2', 'the retry carries the new token');
});

test('a query carries no idempotency key', async () => {
	const seen = [];
	globalThis.fetch = async (url, options) => {
		if (url === '/v1/session') return Response.json({ csrf: 't', csrf_header: 'x-csrf-token' });
		seen.push(JSON.parse(options.body));
		return Response.json({ ok: true, result: {} });
	};
	const { rpc } = await import('../src/lib/api.ts');
	await rpc('fleet.list');
	assert.equal(seen[0].idempotency_key, undefined);
});
