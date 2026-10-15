// The Feed window rule (ia.md "The Feed window") on a scripted owner.feed and a fake clock.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { FeedWindow, PASS_PAGES, isNone } from '../src/lib/feed-window.ts';

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

test('a pass whose tenth page still has more restarts from its first page, in exactly 10 calls', async () => {
	const b = board(1, rows(1));
	const w = new FeedWindow(b.op, fakeClock());
	await w.fresh();
	b.rows.push(...rows(...Array.from({ length: 20 }, (_, i) => i + 2)));
	b.calls = [];
	w.trigger();
	await settle();
	assert.equal(b.calls.length, PASS_PAGES);
	assert.deepEqual([seqs(w), w.view.floor, w.view.exhausted, w.view.restarted], [[21], 20, false, true]);
});
