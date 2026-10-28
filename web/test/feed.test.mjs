// The Feed window rule (ia.md "The Feed window") on a scripted owner.feed and a fake clock.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { test } from 'node:test';
import { FeedWindow, isNone } from '../src/lib/feed-window.ts';

/** A board of rows ({seq, state}) answering owner.feed by bounds, newest first, `size` a page. */
function board(size, rows = []) {
	const b = {
		rows: [...rows],
		calls: [],
		filter: () => true,
		fail: false,
		/** Called on every request, before it is answered. */
		onCall: () => {},
		/** Called once a request has read the rows (a row committed just after it, say). */
		afterRead: () => {},
		op: async (bounds) => {
			b.calls.push(bounds);
			b.onCall(bounds);
			if (b.fail) throw new Error('store unavailable');
			const after = bounds.after ?? 0;
			const before = bounds.before ?? Infinity;
			const match = b.rows.filter((r) => r.seq > after && r.seq < before && b.filter(r)).sort((x, y) => y.seq - x.seq);
			b.afterRead(bounds);
			return { messages: match.slice(0, size).map((r) => ({ ...r })), more: match.length > size };
		}
	};
	return b;
}

function fakeClock() {
	let t = 0;
	const timers = [];
	return {
		now: () => t,
		later: (fn, ms) => timers.push({ at: t + ms, fn }),
		async advance(ms) {
			t += ms;
			for (const due of timers.filter((x) => x.at <= t)) {
				timers.splice(timers.indexOf(due), 1);
				due.fn();
			}
			await settle();
		}
	};
}

const settle = async () => {
	for (let i = 0; i < 20; i++) await new Promise((r) => setImmediate(r));
};
const seqs = (w) => w.view.rows.map((r) => r.seq);
const rows = (...s) => s.map((seq) => ({ seq, state: 'delivered' }));
/** The pass calls among the requests (a pass always names `after`). */
const passes = (b) => b.calls.filter((c) => c.after !== undefined && c.before === undefined).length;

test('a gap of more than two pages is drained, the watermark unchanged until the last page', async () => {
	const b = board(3, rows(1, 2, 3));
	const clock = fakeClock();
	const w = new FeedWindow(b.op, clock);
	await w.fresh();
	assert.deepEqual([w.view.watermark, w.view.exhausted], [3, true]);
	b.rows.push(...rows(4, 5, 6, 7, 8, 9, 10, 11, 12));
	const seen = [];
	b.onCall = () => seen.push(w.view.watermark);
	b.calls = [];
	w.trigger();
	await settle();
	assert.deepEqual(b.calls, [{ after: 0 }, { after: 0, before: 10 }, { after: 0, before: 7 }, { after: 0, before: 4 }]);
	assert.deepEqual(seen, [3, 3, 3, 3], 'the shown watermark waits for the pass to end');
	assert.deepEqual(seqs(w), [12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);
	assert.equal(w.view.watermark, 12);
});

test('an arrival during a pass is caught by the re-run', async () => {
	const b = board(3, rows(1, 2));
	const clock = fakeClock();
	const w = new FeedWindow(b.op, clock);
	await w.fresh();
	let once = true;
	b.afterRead = () => {
		if (!once) return;
		once = false;
		b.rows.push(...rows(3)); // committed just after the pass read; its frame arrives now
		w.trigger();
	};
	w.trigger();
	await settle();
	assert.deepEqual(seqs(w), [2, 1], 'the pass read before the arrival');
	await clock.advance(2000);
	assert.deepEqual(seqs(w), [3, 2, 1], 'the re-run reads it');
});

test('triggers inside 2 s give one pass and one re-run', async () => {
	const b = board(3, rows(1, 2));
	const clock = fakeClock();
	const w = new FeedWindow(b.op, clock);
	await w.fresh();
	b.calls = [];
	for (const at of [0, 500, 500, 500]) {
		await clock.advance(at);
		w.trigger();
		await settle();
	}
	assert.equal(passes(b), 1, 'one pass at once');
	await clock.advance(2000);
	await clock.advance(2000);
	assert.equal(passes(b), 2, 'and one re-run, not one per trigger');
});

test('passes start at least 2 s apart, counted from when a pass begins, not when it is queued', async () => {
	const b = board(3, rows(1, 2));
	const clock = fakeClock();
	const w = new FeedWindow((bounds) => b.op(bounds), clock);
	await w.fresh();
	const begun = [];
	let release = null;
	const op = b.op;
	b.op = async (bounds) => {
		if (bounds.after !== undefined && bounds.before === undefined) begun.push(clock.now());
		if (release === null && bounds.after === undefined && bounds.before === undefined) await new Promise((r) => (release = r));
		return op(bounds);
	};
	void w.fresh(); // a slow fresh window in flight: the pass queues behind it
	w.trigger();
	await clock.advance(1900);
	release();
	await settle();
	assert.deepEqual(begun, [1900], 'the pass began once the fresh window answered');
	await clock.advance(100);
	w.trigger();
	await settle();
	assert.deepEqual(begun, [1900], 'not 100 ms after the last pass began');
	await clock.advance(1900);
	assert.deepEqual(begun, [1900, 3900]);
});

test('a held row on the second loaded page turns read after one pass', async () => {
	const b = board(2, rows(1, 2, 3, 4));
	b.rows[0].state = 'held';
	const w = new FeedWindow(b.op, fakeClock());
	await w.fresh();
	await w.older();
	assert.deepEqual([seqs(w), w.view.exhausted], [[4, 3, 2, 1], true]);
	b.rows[0].state = 'read';
	w.trigger();
	await settle();
	assert.equal(w.view.rows.find((r) => r.seq === 1).state, 'read');
});

test('a resolved problem leaves the list', async () => {
	const b = board(5, rows(1, 2, 3).map((r) => ({ ...r, state: 'held' })));
	b.filter = (r) => r.state === 'held';
	const w = new FeedWindow(b.op, fakeClock());
	await w.fresh();
	b.rows[1].state = 'read';
	w.trigger();
	await settle();
	assert.deepEqual(seqs(w), [3, 1], 'replaced, not merged by id');
});

test('every loaded problem resolved while an older one is below floor gives a fresh window that shows it', async () => {
	const b = board(2, [1, 5, 6].map((seq) => ({ seq, state: 'held' })));
	b.filter = (r) => r.state === 'held';
	const views = [];
	const w = new FeedWindow(b.op, fakeClock(), (v) => views.push(v));
	await w.fresh();
	assert.deepEqual([seqs(w), w.view.floor, w.view.exhausted], [[6, 5], 4, false]);
	b.rows[1].state = 'read';
	b.rows[2].state = 'read';
	w.trigger();
	await settle();
	assert.deepEqual([seqs(w), w.view.exhausted], [[1], true]);
	assert.ok(!views.some(isNone), 'never "No delivery problems" on the way');
});

test('an empty fresh answer without more reads as none; a failed call keeps the window and says so', async () => {
	const empty = new FeedWindow(board(3).op, fakeClock());
	await empty.fresh();
	assert.ok(isNone(empty.view));
	const b = board(3, rows(1, 2));
	const w = new FeedWindow(b.op, fakeClock());
	await w.fresh();
	const asOf = w.view.asOf;
	b.fail = true;
	w.trigger();
	await settle();
	assert.deepEqual([seqs(w), w.view.error, w.view.asOf], [[2, 1], 'store unavailable', asOf]);
	assert.ok(!isNone(w.view), 'never an empty list for a failure');
});

test('a failure after an empty answer is not "Nothing yet"', async () => {
	const b = board(3);
	const w = new FeedWindow(b.op, fakeClock());
	await w.fresh();
	assert.ok(isNone(w.view));
	b.fail = true;
	w.trigger();
	await settle();
	assert.equal(w.view.error, 'store unavailable');
	assert.ok(!isNone(w.view), 'the failure shows, not an empty Feed');
});

/** lib/feed.svelte.ts with its imports given: `$state` as plain values, `rpc` answered by `b`. */
async function feedModule(b, storage) {
	globalThis.localStorage = {
		getItem: (k) => storage[k] ?? null,
		setItem: (k, v) => void (storage[k] = v)
	};
	const listeners = [];
	const source = readFileSync(new URL('../src/lib/feed.svelte.ts', import.meta.url), 'utf8')
		.replace(/^import .*;$/gm, '')
		.replace(/^export /gm, '');
	const mod = new Function('$state', 'rpc', 'onBoard', 'FeedWindow', stripTypeScriptTypes(source) + '\nreturn { Feed, parseFilters };')(
		(x) => x,
		async (_op, body) => b.op(body),
		(fn) => (listeners.push(fn), () => {}),
		FeedWindow
	);
	return { ...mod, frame: (topics) => listeners.forEach((fn) => fn(topics)) };
}

test('a malformed filter part is left out, the others kept', async () => {
	const { parseFilters } = await feedModule(board(3), {});
	const parse = (path) => {
		try {
			return parseFilters(path);
		} catch (e) {
			return `threw ${e.name}`;
		}
	};
	assert.deepEqual(parse('agent=%;dir=out'), { dir: 'out' });
	assert.deepEqual(parse('place=%E0%A4%A;problems=1;agent=desk'), { problems: true, agent: 'desk' });
});

test('the last visit and the seen top never move backward', async () => {
	const b = board(5, rows(1, 2, 3));
	const storage = { 'conde.feed.seen': '50' };
	const clock = fakeClock();
	const { Feed, frame } = await feedModule(b, storage);
	const feed = new Feed({ agent: 'helper' }, clock);
	await settle();
	assert.equal(feed.view.watermark, 3);
	feed.atTop();
	assert.equal(storage['conde.feed.seen'], '50', 'a narrower window does not rewind the last visit');

	b.filter = (r) => r.seq !== 3; // the top row stops matching
	frame(['delivery']);
	await settle();
	assert.equal(feed.view.watermark, 2);
	feed.atTop();
	b.filter = () => true; // and matches again
	frame(['delivery']);
	await clock.advance(2000);
	assert.deepEqual(feed.view.rows.map((r) => r.seq), [3, 2, 1]);
	assert.equal(feed.fresh, 0, 'seq 3 was seen already');
});

test('the mock answers a malformed request with an error, and keeps serving', async (t) => {
	const port = 5800 + Math.floor(Math.random() * 100);
	const mock = spawn(process.execPath, ['scripts/design/mock-server.mjs', 'test', String(port)], { stdio: ['ignore', 'pipe', 'pipe'] });
	t.after(() => mock.kill());
	await new Promise((r) => mock.stdout.once('data', r));
	const post = (body) =>
		fetch(`http://127.0.0.1:${port}/v1/rpc`, { method: 'POST', body }).then(
			(r) => r.json(),
			(e) => ({ error: { code: `no answer (${e.cause?.message ?? e.message})` } })
		);
	const op = (op, body) => post(JSON.stringify({ request_id: 'r', op, body }));
	assert.equal((await op('owner.search', { pattern: '(' })).error?.code, 'bad_request');
	assert.equal((await post('{not json')).error?.code, 'bad_request');
	assert.equal((await op('owner.search', { pattern: 'x' })).ok, true, 'still serving');
});

test('a pass whose tenth page still has more restarts from its first page, in exactly 10 calls', async () => {
	const b = board(1, rows(1));
	const w = new FeedWindow(b.op, fakeClock());
	await w.fresh();
	b.rows.push(...rows(...Array.from({ length: 20 }, (_, i) => i + 2)));
	b.calls = [];
	w.trigger();
	await settle();
	assert.equal(b.calls.length, 10);
	assert.deepEqual([seqs(w), w.view.floor, w.view.exhausted, w.view.restarted], [[21], 20, false, true]);
});
