// The thread answer (ia.md "Thread view", "owner.thread"): continuations merged by id, the tree
// built from `top`, a hidden parent carried through.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ThreadReader, emptyThread, mergeThread, treeOrder } from '../src/lib/thread-tree.ts';

const msg = (id, seq, reply_to, minute = seq, from = 'kdobmayer/desk') => ({
	message_id: id,
	seq,
	reply_to,
	from,
	date: `2026-10-14T12:${String(minute).padStart(2, '0')}:00Z`,
	text: `${id} text\nsecond line`
});

test('a thread with a hidden parent and a continuation has no holes', () => {
	// T replies to something the owner cannot read; A under T; B and C under A; D under C. C is
	// dated before B (siblings go by date, not seq).
	const T = msg('T', 10, 'X'), A = msg('A', 11, 'T'), B = msg('B', 12, 'A', 13), C = msg('C', 13, 'A', 12), D = msg('D', 14, 'C');
	let t = mergeThread(emptyThread(), { top: 'T', start: null, hidden_parent: true, messages: [T, A, B], next: 12 });
	assert.deepEqual([t.hiddenParent, t.next], [true, 12]);
	t = mergeThread(t, { top: 'T', start: null, hidden_parent: true, messages: [A, C, D], next: null });
	const ids = t.messages.map((m) => m.message_id);
	assert.deepEqual(ids, ['T', 'A', 'B', 'C', 'D'], 'each once, by seq (A came twice)');
	for (const m of t.messages) assert.ok(m.message_id === t.top || ids.includes(m.reply_to), `${m.message_id}'s parent`);
	assert.equal(t.next, null);

	const tree = treeOrder(t.messages, t.top).map((l) => [l.message.message_id, l.depth, l.re?.line ?? null]);
	assert.deepEqual(tree, [
		['T', 0, null],
		['A', 0, null],
		['C', 1, null],
		['D', 1, null],
		['B', 1, 'A text']
	]);
});

// A server over a linear chain of `n` messages, answering owner.thread in parts of 200 with
// `next` as the contract gives it; `hold` keeps the next first-part answer until released.
function chainServer(n) {
	const rows = Array.from({ length: n }, (_, i) => ({ ...msg(`M${i + 1}`, i + 1, i ? `M${i}` : null, 0), delivery: { recipients: [{ recipient: 'r', state: 'delivered' }], read_by: null } }));
	const part = (from) => {
		const page = rows.filter((m) => m.seq > from).slice(0, 200);
		const more = rows.some((m) => m.seq > (page.at(-1)?.seq ?? from));
		return { top: 'M1', start: null, hidden_parent: false, messages: page.map((m) => structuredClone(m)), next: more ? page.at(-1).seq : null };
	};
	const s = { rows, calls: [], held: null };
	s.op = async (body) => {
		s.calls.push(body);
		const answer = part(body.after ?? 0);
		if (body.after === undefined && s.hold) {
			s.hold = false;
			await new Promise((release) => (s.held = release));
		}
		return answer;
	};
	return s;
}

test('a refresh after the continuation re-reads the whole span: next stays null, a later row updates', async () => {
	const server = chainServer(201);
	const t = new ThreadReader('M1', server.op);
	await t.load();
	assert.deepEqual([t.state.messages.length, t.state.next], [200, 200]);
	await t.more();
	assert.deepEqual([t.state.messages.length, t.state.next], [201, null]);
	server.rows[200].delivery = { recipients: [{ recipient: 'r', state: 'read' }], read_by: null };
	await t.load();
	assert.equal(t.state.next, null, 'the continuation frontier is kept');
	assert.equal(t.state.messages.length, 201);
	assert.equal(t.state.messages[200].delivery.recipients[0].state, 'read', 'the later row is re-read');
	assert.deepEqual(server.calls.slice(-2), [{ id: 'M1' }, { id: 'M1', after: 200 }]);
});

test('a first-part answer that arrives after a continuation does not undo it', async () => {
	const server = chainServer(201);
	const t = new ThreadReader('M1', server.op);
	await t.load();
	server.hold = true;
	const refresh = t.load();
	const more = t.more();
	await new Promise((r) => setTimeout(r, 10));
	server.held();
	await Promise.all([refresh, more]);
	assert.deepEqual([t.state.messages.length, t.state.next], [201, null]);
});
