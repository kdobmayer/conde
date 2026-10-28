// The Feed page's derived values (ia.md "Feed", "Thread view"): URL forms, day sections, the
// last-visit divider, the thread's headline and time span, the filters in words.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { describeFilters, dividerBefore, feedHash, feedSection, headline, threadHash, timeSpan } from '../src/lib/feed-view.ts';

process.env.TZ = 'Europe/Budapest';

test('the Feed and thread URLs carry the filters in the path form', () => {
	assert.equal(feedHash(''), '#/');
	assert.equal(feedHash('agent=desk;dir=out'), '#/f/agent=desk;dir=out');
	assert.equal(threadHash('', '01M4'), '#/t/01M4');
	assert.equal(threadHash('problems=1', '01M4'), '#/f/problems=1/t/01M4');
});

test('rows fall under the last hour, earlier today, yesterday, then their date', () => {
	const now = Date.parse('2026-10-14T12:20:00Z');
	const ago = (min) => new Date(now - min * 60_000).toISOString();
	assert.equal(feedSection(ago(59), now), 'In the last hour');
	assert.equal(feedSection(ago(-1), now), 'In the last hour', 'a clock ahead of ours');
	assert.equal(feedSection(ago(61), now), 'Earlier today');
	assert.equal(feedSection('2026-10-13T22:01:00Z', now), 'Earlier today', 'after local midnight (UTC+2), before UTC midnight');
	assert.equal(feedSection('2026-10-13T21:59:00Z', now), 'Yesterday', 'before local midnight');
	assert.equal(feedSection('2026-10-13T12:00:00Z', now), 'Yesterday');
	assert.match(feedSection('2026-10-10T12:00:00Z', now), /10/);
	assert.match(feedSection('2025-10-10T12:00:00Z', now), /2025/);
});

test('the divider sits above the first row already shown, only with new rows above it', () => {
	const rows = [{ seq: 30 }, { seq: 20 }, { seq: 10 }];
	assert.equal(dividerBefore(rows, 20), 1);
	assert.equal(dividerBefore(rows, 25), 1);
	assert.equal(dividerBefore(rows, 30), -1, 'nothing new');
	assert.equal(dividerBefore(rows, 5), -1, 'everything new: no row below it');
	assert.equal(dividerBefore(rows, 0), -1, 'no last visit on this device');
});

test("a thread's headline is its top message's first sentence, else its first line cut", () => {
	assert.equal(headline('Review of batch 12-15: READY WITH P3s. 4ad162e, 6456219 checked.'), 'Review of batch 12-15: READY WITH P3s.');
	assert.equal(headline('\nRelease 1.2 is out\nmore'), 'Release 1.2 is out');
	assert.equal(headline('x'.repeat(100), 20), `${'x'.repeat(19)}…`);
	assert.equal(headline('v1.2.3 shipped'), 'v1.2.3 shipped', 'a dot inside a word ends no sentence');
});

test('the time span names dates only when it crosses a day', () => {
	const clock = (iso) => iso.slice(11, 16);
	assert.equal(timeSpan('2026-10-14T09:10:00Z', '2026-10-14T11:32:00Z', clock), '09:10–11:32');
	assert.equal(timeSpan('2026-10-14T09:10:00Z', '2026-10-14T09:10:00Z', clock), '09:10');
	assert.match(timeSpan('2026-10-13T09:10:00Z', '2026-10-14T11:32:00Z', clock), /13.*09:10 – .*14.*11:32/);
});

test('the active filters in words', () => {
	assert.equal(describeFilters({ agent: 'desk', dir: 'out', place: 'dm', problems: true }), 'agent desk · sent · direct messages · needs attention');
	assert.equal(describeFilters({ place: 'conde-dev' }), '#conde-dev');
	assert.equal(describeFilters({}), '');
});
