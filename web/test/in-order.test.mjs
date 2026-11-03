// A read sent again on every change (the rail's owner.list), with answers held and released in
// any order: an older answer never lands over a newer one or over a local write, and a newer read
// in flight never holds an older answer back.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { test } from 'node:test';
import { inOrder } from '../src/lib/in-order.ts';
import { withNotify } from '../src/lib/notify.ts';

/** Reads whose answers wait until the test settles them; `shown` is what reached the screen. */
function reads() {
	const r = { held: [], shown: [] };
	r.order = inOrder(
		() =>
			new Promise((resolve, reject) => {
				r.held.push({ resolve, reject });
			}),
		(answer) => r.shown.push(answer),
		(e) => r.shown.push(`error: ${e.message}`)
	);
	return r;
}
const flush = () => new Promise((resolve) => setImmediate(resolve));

test('the first change’s read, answered after the second change’s, never lands', async () => {
	const r = reads();
	const first = r.order.refresh(); // after the change to every post
	const second = r.order.refresh(); // after the change back to mentions
	r.held[1].resolve('mentions');
	await second;
	r.held[0].resolve('all');
	await first;
	assert.deepEqual(r.shown, ['mentions']);
});

test('answers in order all land', async () => {
	const r = reads();
	const first = r.order.refresh();
	const second = r.order.refresh();
	r.held[0].resolve('a');
	await first;
	r.held[1].resolve('b');
	await second;
	assert.deepEqual(r.shown, ['a', 'b']);
});

test('a newer read in flight does not hold an older answer back', async () => {
	const r = reads();
	const pending = [];
	for (let i = 0; i < 3; i++) {
		pending.push(r.order.refresh());
		if (i > 0) r.held[i - 1].resolve(`answer ${i - 1}`);
		await flush();
	}
	assert.deepEqual(r.shown, ['answer 0', 'answer 1'], 'each lands while the next is in flight');
	r.held[2].resolve('answer 2');
	await Promise.all(pending);
	assert.deepEqual(r.shown, ['answer 0', 'answer 1', 'answer 2']);
});

test('a local write drops the answers of reads sent before it, not of reads sent after', async () => {
	const r = reads();
	const before = r.order.refresh();
	r.order.wrote();
	const after = r.order.refresh();
	r.held[0].resolve('before the write');
	await before;
	assert.deepEqual(r.shown, []);
	r.held[1].resolve('after the write');
	await after;
	assert.deepEqual(r.shown, ['after the write']);
});

test('a failure lands like an answer: an older one never covers a newer answer', async () => {
	const r = reads();
	const first = r.order.refresh();
	const second = r.order.refresh();
	r.held[1].resolve('fresh');
	await second;
	r.held[0].reject(new Error('the store did not answer'));
	await first;
	assert.deepEqual(r.shown, ['fresh']);
	const third = r.order.refresh();
	r.held[2].reject(new Error('the store did not answer'));
	await third;
	assert.deepEqual(r.shown, ['fresh', 'error: the store did not answer']);
});

test('the rail: two quick changes, the first one’s read answered last; the rail keeps the board’s setting', async () => {
	const api = await import('../src/lib/api.ts');
	/** owner.list answers held until released with the board's setting at the time it read. */
	const held = [];
	globalThis.fetch = async (url) => {
		if (url === '/v1/session') return Response.json({ user: 'k', display: 'k', csrf: 't', csrf_header: 'x-csrf-token' });
		return new Promise((resolve) => held.push((notify) => resolve(Response.json({ ok: true, result: rail(notify) }))));
	};
	const rail = (notify) => ({ owner: 'k', agents: [{ name: 'k/desk', kind: 'claude', address: null, dms: [], channels: [ch('#dev', notify)] }] });
	const ch = (channel, notify) => ({ channel, topic: null, last_seq: 1, last_at: null, unread: 0, notify });
	globalThis.location = { replace: () => {}, protocol: 'http:', host: 'x', hash: '' };
	// board.svelte.ts as the app runs it, its imports handed in
	const source = readFileSync(new URL('../src/lib/board.svelte.ts', import.meta.url), 'utf8')
		.replace(/^import .*;$/gm, '')
		.replace(/^export /gm, '');
	const b = new Function('$state', 'loadSession', 'rpc', 'signIn', 'onSession', 'currentSession', 'inOrder',
		stripTypeScriptTypes(source) + '\nreturn {board, refreshRail, changeRail};')(
		(x) => x, api.loadSession, api.rpc, () => {}, api.onSession, api.currentSession, inOrder);
	await api.loadSession();
	const shown = () => b.board.rail.agents[0].channels[0].notify;
	const answered = async (n, notify) => {
		while (held.length <= n) await flush();
		held[n](notify);
	};

	const start = b.refreshRail();
	await answered(0, 'mentions');
	await start;
	const stale = b.refreshRail(); // a board change's read, sent before the first change
	b.changeRail((r) => withNotify(r, 'k/desk', '#dev', 'all'));
	const first = b.refreshRail();
	await answered(1, 'mentions');
	await stale;
	assert.equal(shown(), 'all', 'a read sent before the change never lands over it');
	b.changeRail((r) => withNotify(r, 'k/desk', '#dev', 'mentions'));
	const second = b.refreshRail();
	await answered(3, 'mentions');
	await second;
	await answered(2, 'all');
	await first;
	assert.equal(shown(), 'mentions', 'the first change’s read, answered last, never lands');
});
