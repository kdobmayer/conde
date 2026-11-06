// Drives the app's Feed and Fleet pages headless on the mock API: the filter menu, the thread
// beside the Feed and in the sheet, the disclosure, the error line, the live announcer, the
// agent list's resize handle and per-channel push setting, and markdown in message bodies, by
// keyboard at 1440×900 (and the overlay layout at 1180×820) and by touch at 390×844. Exits
// non-zero when a check fails. Serves the built app through mock-server.mjs.
// Usage: node scripts/design/interact.mjs [build-dir]
// Needs playwright-core 1.64.0 (npm i --no-save) and a Chromium headless shell (CHROME, as in
// shoot.mjs).

import { spawn } from 'node:child_process';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const here = dirname(fileURLToPath(import.meta.url));
const port = 5900 + Math.floor(Math.random() * 90);
const args = [join(here, 'mock-server.mjs'), ...(process.argv[2] ? [process.argv[2]] : [join(here, '../../../crates/conde-remote/web-dist')]), String(port)];
const mock = spawn(process.execPath, args, { stdio: ['ignore', 'pipe', 'inherit'] });
await new Promise((resolve) => mock.stdout.once('data', resolve));

const executablePath =
	process.env.CHROME ??
	`${homedir()}/.cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell`;
const browser = await chromium.launch({ executablePath, args: ['--disable-gpu'] });

let failed = 0;
/** The size the checks run in, printed with each. */
let where = '';
async function check(name, fn) {
	try {
		await fn();
		console.log(`ok      [${where}] ${name}`);
	} catch (e) {
		failed++;
		console.log(`not ok  [${where}] ${name}: ${e.message.split('\n')[0]}`);
	}
}
function expect(cond, what) {
	if (!cond) throw new Error(what);
}

/** Facts about the focused element and the page, read in the browser. */
const focus = (page) =>
	page.evaluate(() => {
		const a = document.activeElement;
		const box = a?.closest('[data-test]')?.getAttribute('data-test') ?? null;
		return {
			tag: a?.tagName.toLowerCase(),
			text: (a?.textContent ?? '').trim().slice(0, 60),
			test: a?.getAttribute('data-test') ?? box,
			inDialog: !!a?.closest('dialog'),
			role: a?.getAttribute('role'),
			active: a?.getAttribute('aria-activedescendant') ? document.getElementById(a.getAttribute('aria-activedescendant'))?.textContent?.trim() : null
		};
	});
const listboxes = (page) => page.locator('[role=listbox]').count();
const settle = (page) => page.waitForTimeout(400);
const box = (page, sel) => page.locator(sel).boundingBox();

// ---- the Feed (U9): rows, the thread beside it, the sheet below 1280 px ----
const FEED = `http://127.0.0.1:${port}/`;
// the fixture's clock, as in shoot.mjs, so day groups and times are stable
const NOW = new Date('2026-10-14T12:20:00Z');

/** A page on the Feed at `hash`, with every owner.feed call it makes counted. `fixed` holds the
 * clock at the fixture's NOW; a check that needs a refresh pass runs on the real clock, since
 * the pass cadence is measured on it. */
async function openFeed(size, hash = '#/', { fixed = true, label = 'feed', wait = '.col [data-id]' } = {}) {
	where = `${label} ${size.width}`;
	const context = await browser.newContext({
		viewport: size,
		...(size.width < 720 ? { isMobile: true, hasTouch: true } : {}),
		locale: 'en-GB',
		timezoneId: 'Europe/Budapest',
		reducedMotion: 'reduce'
	});
	const page = await context.newPage();
	if (fixed) await page.clock.setFixedTime(NOW);
	// CSP violations, for the checks that render message bodies
	await page.addInitScript(() => {
		window.__csp = [];
		document.addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective));
	});
	page.on('pageerror', (e) => {
		failed++;
		console.log(`not ok  page error: ${e.message}`);
	});
	const calls = { feed: 0 };
	page.on('request', (r) => {
		if (r.url().endsWith('/v1/rpc') && JSON.parse(r.postData() ?? '{}').op === 'owner.feed') calls.feed++;
	});
	await page.goto(FEED + hash, { waitUntil: 'networkidle' });
	await page.waitForSelector(wait);
	// the socket's first frame starts one refresh pass; let it land before counting
	await page.waitForTimeout(800);
	return { context, page, calls };
}

const hashOf = (page) => page.evaluate(() => location.hash);
const sideOpen = (page) => page.locator('aside.side').isVisible();
const feedSheetOpen = (page) => page.evaluate(() => document.querySelector('.feedpage dialog')?.open ?? false);
/** The row (by index) scrolled to the top of the list, under the sticky head. */
async function scrollRowToTop(page, index) {
	return page.evaluate((i) => {
		const col = document.querySelector('.col');
		const row = col.querySelectorAll('a.row')[i];
		const head = col.querySelector('.head');
		const edge = Math.max(col.getBoundingClientRect().top, head.getBoundingClientRect().bottom);
		col.scrollTop += row.getBoundingClientRect().top - edge - 20;
		return row.dataset.id;
	}, index);
}
const rowTop = (page, id) => page.evaluate((id) => document.querySelector(`.col [data-id="${id}"]`).getBoundingClientRect().top, id);
const focusedRow = (page) => page.evaluate(() => document.activeElement?.closest('.col [data-id]')?.getAttribute('data-id') ?? null);

{
	const { context, page, calls } = await openFeed({ width: 1440, height: 900 });
	let id;
	let top;
	const before = calls.feed;
	await check('feed: a row reads time, direction word, sender → recipient, place; no totals or reply counts', async () => {
		const row = page.locator('.col a.row').first();
		const kick = (await row.locator('.kick').textContent()).replace(/\s+/g, ' ').trim();
		expect(/^\d\d:\d\d (Received|Sent|Between yours) \S+ to \S+ (Direct|#\S+)$/.test(kick), kick);
		expect((await row.locator('.kick svg.arrow').count()) === 1, 'the arrow is not an inline SVG');
		const bar = await page.locator('.filters').textContent();
		expect(!/\d/.test(bar), `a number in the filter bar: ${bar}`);
		const items = (await page.locator('.col a.row .meta > *').allTextContents()).map((t) => t.trim());
		expect(items.every((t) => !/\d.*repl|repl.*\d/i.test(t)), `a reply count on a row: ${items.join(', ')}`);
	});
	await check('feed: day groups head the rows', async () => {
		const heads = await page.locator('.col h2').allTextContents();
		expect(heads[0] === 'In the last hour' && heads.includes('Earlier today'), heads.join(' | '));
	});
	await check('feed: Enter on a row opens its thread beside the Feed, focus on the thread', async () => {
		id = await scrollRowToTop(page, 9);
		top = await rowTop(page, id);
		await page.locator(`.col [data-id="${id}"]`).focus();
		await page.keyboard.press('Enter');
		await page.waitForSelector('aside.side .thread ol li');
		expect((await hashOf(page)) === `#/t/${id}`, await hashOf(page));
		expect((await page.locator(`aside.side li[data-id="${id}"]`).getAttribute('aria-current')) === 'true', 'the selected message is not marked');
		expect(await page.evaluate(() => !!document.activeElement?.closest('aside.side')), 'focus did not move to the thread');
		const b = await box(page, 'aside.side');
		expect(Math.round(b.width) === 500 && Math.round(b.x + b.width) === 1440, JSON.stringify(b));
	});
	await check('feed: Escape closes it; focus returns to that row at the same scroll position', async () => {
		await page.keyboard.press('Escape');
		await page.waitForFunction(() => !document.querySelector('aside.side'));
		await settle(page);
		expect((await focusedRow(page)) === id, `focus on ${await focusedRow(page)}`);
		const now = await rowTop(page, id);
		expect(Math.abs(now - top) < 2, `row moved from ${top} to ${now}`);
		expect((await hashOf(page)) === '#/', await hashOf(page));
	});
	await check('feed: browser back closes the thread beside the Feed', async () => {
		await page.locator(`.col [data-id="${id}"]`).click();
		await page.waitForSelector('aside.side');
		await page.goBack();
		await page.waitForFunction(() => !document.querySelector('aside.side'));
		// the rows change layout with the column's width; the list restores its anchor after that
		await settle(page);
		expect((await hashOf(page)) === '#/', await hashOf(page));
		expect(Math.abs((await rowTop(page, id)) - top) < 2, 'scroll position not kept');
	});
	await check('feed: opening and closing threads made no owner.feed call', async () => {
		expect(calls.feed === before, `${calls.feed - before} call(s)`);
	});
	await check('feed: a filter by keyboard goes into the URL; back restores the Feed', async () => {
		const all = await page.locator('.col a.row').count();
		await page.locator('.filters .menu').first().locator('button').focus();
		await page.keyboard.press('Enter');
		await page.keyboard.press('ArrowDown');
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => location.hash === '#/f/agent=desk');
		await settle(page);
		const rows = await page.locator('.col a.row').count();
		expect(rows > 0 && rows < all, `${rows} of ${all}`);
		await page.goBack();
		await page.waitForFunction(() => location.hash === '#/');
		await settle(page);
		expect((await page.locator('.col a.row').count()) === all, 'filters not cleared');
	});
	await check('feed: needs attention lists problem rows only', async () => {
		await page.getByRole('button', { name: 'Needs attention' }).click();
		await page.waitForFunction(() => location.hash === '#/f/problems=1');
		await settle(page);
		const words = await page.locator('.col a.row .meta .state').allTextContents();
		expect(words.length > 0 && words.every((w) => ['retrying', 'held', 'denied', 'refused', 'gone'].includes(w)), words.join(','));
		await page.goBack();
		await page.waitForFunction(() => location.hash === '#/');
	});
	await check('feed: search replaces the list until the field is cleared', async () => {
		await page.locator('input.search').fill('barrier');
		await page.waitForSelector('.col a.hit');
		expect((await page.locator('.filters').count()) === 0, 'filters still shown in search');
		const lines = await page.locator('.col a.hit .line').allTextContents();
		expect(lines.length > 0 && lines.every((l) => /barrier/i.test(l)), lines.join(' | '));
		await page.locator('input.search').fill('');
		await page.waitForSelector('.col a.row');
		expect((await page.locator('.filters').count()) === 1, 'filters not back');
	});
	await context.close();
}

{
	const thread = await import('../../src/lib/design/fixture.ts').then((fx) => fx.ids.feedThread);
	const { context, page } = await openFeed({ width: 1440, height: 900 }, `#/t/${thread}`);
	await check('feed: a #/t/<id> deep link opens the Feed with that thread and its row selected', async () => {
		await page.waitForSelector(`aside.side li[data-id="${thread}"]`);
		expect((await hashOf(page)) === `#/t/${thread}`, await hashOf(page));
		expect((await page.locator(`.col a.row[data-id="${thread}"]`).getAttribute('aria-current')) === 'true', 'row not selected');
	});
	await check('feed: Close on a deep link stays in the app', async () => {
		await page.locator('aside.side .close').click();
		await page.waitForFunction(() => !document.querySelector('aside.side'));
		expect((await hashOf(page)) === '#/', await hashOf(page));
	});
	await context.close();
}

{
	const { context, page, calls } = await openFeed({ width: 1180, height: 820 });
	await check('feed at 1180: a row opens the thread as a 500 px overlay; Escape returns focus to the row in place', async () => {
		const id = await scrollRowToTop(page, 6);
		const top = await rowTop(page, id);
		const before = calls.feed;
		await page.locator(`.col [data-id="${id}"]`).focus();
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
		await page.waitForSelector('.feedpage dialog .thread ol li');
		const b = await box(page, '.feedpage dialog');
		expect(Math.round(b.width) === 500 && Math.round(b.x + b.width) === 1180, JSON.stringify(b));
		await settle(page);
		await page.keyboard.press('Escape');
		await page.waitForFunction(() => !document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect((await focusedRow(page)) === id, `focus on ${await focusedRow(page)}`);
		expect(Math.abs((await rowTop(page, id)) - top) < 2, 'scroll position not kept');
		expect((await hashOf(page)) === '#/', await hashOf(page));
		expect(calls.feed === before, 'the Feed was re-read');
	});
	await context.close();
}

{
	const { context, page, calls } = await openFeed({ width: 390, height: 844 });
	let id;
	let top;
	await check('feed by touch: a tap on a row opens the thread in a bottom sheet', async () => {
		id = await scrollRowToTop(page, 5);
		top = await rowTop(page, id);
		await page.locator(`.col [data-id="${id}"]`).tap();
		await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
		await page.waitForSelector(`.feedpage dialog li[data-id="${id}"]`);
		const b = await box(page, '.feedpage dialog');
		expect(Math.round(b.width) === 390 && Math.round(b.y + b.height) === 844 && b.height < 844 * 0.8, JSON.stringify(b));
		expect((await hashOf(page)) === `#/t/${id}`, await hashOf(page));
	});
	await check('feed by touch: back closes the sheet, the Feed where it was', async () => {
		const before = calls.feed;
		await settle(page);
		await page.goBack();
		await page.waitForFunction(() => !document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect((await hashOf(page)) === '#/', await hashOf(page));
		expect((await focusedRow(page)) === id, `focus on ${await focusedRow(page)}`);
		expect(Math.abs((await rowTop(page, id)) - top) < 2, 'scroll position not kept');
		expect(calls.feed === before, 'the Feed was re-read');
	});
	await check('feed by touch: Close closes the sheet too; reopening works', async () => {
		await page.locator(`.col [data-id="${id}"]`).tap();
		await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		await page.locator('.feedpage dialog header button').tap();
		await page.waitForFunction(() => !document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect((await hashOf(page)) === '#/', await hashOf(page));
		await page.locator(`.col [data-id="${id}"]`).tap();
		await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
	});
	await context.close();
}

// ---- the filter menu, on the Feed's filter bar (keyboard, 1440) ----
const menu = (page, i) => page.locator('.filters .menu').nth(i);
const menuOf = (page) => page.evaluate(() => [...document.querySelectorAll('.filters .menu')].indexOf(document.activeElement?.closest('.filters .menu')));
{
	const { context, page } = await openFeed({ width: 1440, height: 900 }, '#/', { label: 'menu' });
	const place = menu(page, 1).locator('button.trigger');
	await check('filter menu: Tab reaches the next menu button', async () => {
		await menu(page, 0).locator('button.trigger').focus();
		await page.keyboard.press('Tab');
		const f = await focus(page);
		expect((await menuOf(page)) === 1 && f.tag === 'button', `focus on ${JSON.stringify(f)}`);
	});
	await check('filter menu: Enter opens at the selected option, focus in the list', async () => {
		await page.keyboard.press('Enter');
		const f = await focus(page);
		expect((await listboxes(page)) === 1, 'no listbox');
		expect((await place.getAttribute('aria-expanded')) === 'true', 'aria-expanded not true');
		expect(f.role === 'listbox' && f.active === 'Every place', `focus ${JSON.stringify(f)}`);
	});
	await check('filter menu: arrows, Home and End move; ArrowUp stops at the top', async () => {
		const seen = [];
		for (const key of ['ArrowDown', 'ArrowDown', 'ArrowUp', 'End', 'Home', 'ArrowUp']) {
			await page.keyboard.press(key);
			seen.push((await focus(page)).active);
		}
		expect(seen.join(',') === 'Direct messages,#conde-dev,Direct messages,#lobby,Every place,Every place', seen.join(','));
	});
	await check('filter menu: Enter selects into the URL, closes and returns focus to the button', async () => {
		await page.keyboard.press('ArrowDown');
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => location.hash === '#/f/place=dm');
		await settle(page);
		const f = await focus(page);
		expect((await listboxes(page)) === 0, 'list still open');
		expect((await menuOf(page)) === 1 && f.tag === 'button', `focus ${JSON.stringify(f)}`);
		expect((await place.textContent()).includes('Direct messages'), 'button does not show the value');
	});
	await check('filter menu: Space opens; Escape closes without a change, focus on the button', async () => {
		await page.keyboard.press(' ');
		expect((await focus(page)).active === 'Direct messages', 'opened away from the selected option');
		await page.keyboard.press('ArrowDown');
		await page.keyboard.press('Escape');
		const f = await focus(page);
		expect((await listboxes(page)) === 0, 'list still open');
		expect((await menuOf(page)) === 1 && f.tag === 'button', `focus ${JSON.stringify(f)}`);
		expect((await hashOf(page)) === '#/f/place=dm', await hashOf(page));
	});
	await check('filter menu: ArrowDown opens; Tab closes it and moves on to the next control', async () => {
		await page.keyboard.press('ArrowDown');
		expect((await listboxes(page)) === 1, 'ArrowDown did not open');
		await page.keyboard.press('Tab');
		expect((await listboxes(page)) === 0, 'Tab left it open');
		const f = await focus(page);
		expect(f.tag === 'button' && f.text === 'All' && (await menuOf(page)) === -1, `focus ${JSON.stringify(f)}`);
	});
	await check('filter menu: a press outside closes it', async () => {
		await place.focus();
		await page.keyboard.press('Enter');
		await page.mouse.click(700, 880);
		expect((await listboxes(page)) === 0, 'still open');
	});
	await context.close();
}

// ---- the sheet, as the Feed's thread overlay (keyboard, 1180) ----
const sheetOpen = (page) => page.evaluate(() => document.querySelector('.feedpage dialog')?.open ?? false);
{
	const { context, page } = await openFeed({ width: 1180, height: 820 }, '#/', { label: 'sheet' });
	const id = await scrollRowToTop(page, 3);
	const openRow = async () => {
		await page.locator(`.col [data-id="${id}"]`).focus();
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
		await page.waitForSelector('.feedpage dialog .thread ol li');
		await settle(page);
	};
	await check('sheet: Enter opens it as a modal dialog with focus inside', async () => {
		await openRow();
		const f = await focus(page);
		expect(f.inDialog, `focus ${JSON.stringify(f)}`);
	});
	await check('sheet: Tab and Shift+Tab stay inside', async () => {
		for (const key of [...Array(8).fill('Tab'), ...Array(8).fill('Shift+Tab')]) {
			await page.keyboard.press(key);
			const f = await focus(page);
			expect(f.inDialog, `after ${key}: ${JSON.stringify(f)}`);
		}
	});
	await check('sheet: the page behind is inert', async () => {
		const other = await page.evaluate((id) => [...document.querySelectorAll('.col a.row')].find((r) => r.dataset.id !== id && r.getBoundingClientRect().top > 200)?.dataset.id, id);
		const b = await box(page, `.col [data-id="${other}"]`);
		await page.mouse.click(Math.min(b.x + 40, 600), b.y + b.height / 2);
		await settle(page);
		expect((await hashOf(page)) === `#/t/${id}`, `the row behind took the click: ${await hashOf(page)}`);
		expect(await sheetOpen(page), 'a click behind closed it');
	});
	await check('sheet: nothing animates (reduced motion)', async () => {
		const n = await page.evaluate(() => document.getAnimations().length);
		expect(n === 0, `${n} animations`);
	});
	await check('sheet: browser back closes it; focus returns to the opener', async () => {
		await page.goBack();
		await page.waitForFunction(() => !document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect((await focusedRow(page)) === id, `focus on ${await focusedRow(page)}`);
		expect((await hashOf(page)) === '#/', await hashOf(page));
	});
	await check('sheet: Close closes it; reopening still works', async () => {
		await openRow();
		await page.locator('.feedpage dialog header button').click();
		await page.waitForFunction(() => !document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect((await focusedRow(page)) === id, 'focus not on the opener');
		expect((await hashOf(page)) === '#/', await hashOf(page));
		await openRow();
		await page.keyboard.press('Escape');
		await page.waitForFunction(() => !document.querySelector('.feedpage dialog')?.open);
		await settle(page);
	});
	await check('sheet: closed by the page when the window widens past 1280 px, it leaves no history entry behind', async () => {
		await openRow();
		await page.setViewportSize({ width: 1440, height: 900 });
		await page.waitForSelector('aside.side .thread ol li');
		await settle(page);
		expect(!(await sheetOpen(page)), 'the sheet stayed open beside the panel');
		expect((await hashOf(page)) === `#/t/${id}`, await hashOf(page));
		// the sheet's entry is gone: one back leaves the thread's own entry (it replaced the
		// Feed's when it opened in the sheet), it does not land on a copy of it
		await page.goBack();
		await settle(page);
		expect((await page.evaluate(() => location.href)) !== `${FEED}#/t/${id}`, 'back stayed on the thread: a stale sheet entry');
	});
	await context.close();
}
{
	const thread = await import('../../src/lib/design/fixture.ts').then((fx) => fx.ids.feedThread);
	const { context, page } = await openFeed({ width: 390, height: 844 }, `#/t/${thread}`, { label: 'sheet' });
	await check('sheet: mounted open by a deep link, it stays open; Close returns to the Feed in the app', async () => {
		await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect(await sheetOpen(page), 'closed itself');
		await page.locator('.feedpage dialog header button').tap();
		await page.waitForFunction(() => !document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect((await hashOf(page)) === '#/', `left the Feed: ${page.url()}`);
	});
	await context.close();
}

// ---- the disclosure, on the Fleet's teammates ----
for (const size of [
	{ width: 1440, height: 900 },
	{ width: 390, height: 844 }
]) {
	const { context, page } = await openFeed(size, '#/fleet', { label: 'fleet', wait: 'button[aria-expanded]:has-text("konstantin")' });
	const toggle = page.locator('button[aria-expanded]', { hasText: 'konstantin' });
	const region = async () => page.locator(`#${await toggle.getAttribute('aria-controls')}`);
	if (size.width > 720) {
		await check('disclosure: Enter and Space toggle aria-expanded and the region', async () => {
			await toggle.focus();
			expect((await toggle.getAttribute('aria-expanded')) === 'false' && !(await (await region()).isVisible()), 'not closed at first');
			await page.keyboard.press('Enter');
			expect((await toggle.getAttribute('aria-expanded')) === 'true' && (await (await region()).isVisible()), 'Enter did not open it');
			await page.keyboard.press(' ');
			expect((await toggle.getAttribute('aria-expanded')) === 'false' && !(await (await region()).isVisible()), 'Space did not close it');
		});
	} else {
		await check('disclosure by touch: tap toggles', async () => {
			await toggle.tap();
			expect((await toggle.getAttribute('aria-expanded')) === 'true', 'tap did not open');
			await toggle.tap();
			expect((await toggle.getAttribute('aria-expanded')) === 'false', 'tap did not close');
		});
	}
	await context.close();
}

// ---- the error line and the announcer, on a Feed refresh pass (real clock) ----
/** Starts a refresh pass the way a return to the tab does. */
const pass = (page) => page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
{
	const { context, page } = await openFeed({ width: 1440, height: 900 }, '#/', { fixed: false, label: 'pass' });
	let failing = false;
	await page.route('**/v1/rpc', async (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (failing && op === 'owner.feed') return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: false, error: { code: 'unavailable', message: 'store unavailable' } }) });
		return route.fallback();
	});
	await check('error line: a failed pass keeps the rows and says "as of hh:mm"; Retry by keyboard reloads', async () => {
		const rows = await page.locator('.col a.row').count();
		failing = true;
		await pass(page);
		const alert = page.getByRole('alert');
		await alert.waitFor({ timeout: 6000 });
		expect(/as of \d\d:\d\d/.test(await alert.textContent()), `no "as of hh:mm": ${await alert.textContent()}`);
		expect((await page.locator('.col a.row').count()) === rows, 'the rows did not stay');
		failing = false;
		await alert.getByRole('button', { name: 'Retry' }).focus();
		await page.keyboard.press('Enter');
		await alert.waitFor({ state: 'detached', timeout: 6000 });
	});
	await page.unroute('**/v1/rpc');
	await page.route('**/v1/rpc', async (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (op !== 'owner.feed') return route.fallback();
		const res = await route.fetch();
		const body = await res.json();
		const top = body.result?.messages?.[0];
		if (top && !JSON.parse(route.request().postData()).body?.before) {
			body.result.messages.unshift({ ...top, message_id: 'ARRIVED-1', seq: top.seq + 1000, text: 'Arrived during the check.' });
		}
		return route.fulfill({ response: res, body: JSON.stringify(body) });
	});
	await check('announcer: arrivals are said in a polite live region', async () => {
		await settle(page);
		await pass(page);
		await page.waitForFunction(() => /new message/.test([...document.querySelectorAll('[aria-live=polite]')].map((e) => e.textContent).join(' ') ?? ''), null, { timeout: 6000 });
	});
	await context.close();
}

// ---- an arrival above the reading position leaves the rows on screen where they were ----
for (const size of [
	{ width: 1280, height: 800 },
	{ width: 390, height: 844 }
]) {
	const { context, page } = await openFeed(size, '#/', { fixed: false, label: 'arrival' });
	let template = null;
	let arrive = false;
	await page.route('**/v1/rpc', async (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (op !== 'owner.feed') return route.fallback();
		const res = await route.fetch();
		const body = await res.json();
		template ??= body.result?.messages?.[0];
		if (template && arrive && !JSON.parse(route.request().postData()).body?.before) {
			const text = 'Arrived while the reader was further down; long enough to take more than one line in the narrow column.';
			body.result.messages.unshift({ ...template, message_id: 'ARRIVED-3', seq: template.seq + 1000, text });
		}
		return route.fulfill({ response: res, body: JSON.stringify(body) });
	});
	// reloaded with the route in place, so the first answer gives the arrival's template
	await page.reload({ waitUntil: 'networkidle' });
	await page.waitForSelector('.col [data-id]');
	await check('arrival: one message arriving above keeps the row in view within 1 px', async () => {
		const id = await scrollRowToTop(page, 9);
		await settle(page);
		const top = await rowTop(page, id);
		arrive = true;
		await pass(page);
		await page.waitForSelector('.col a.row[data-id="ARRIVED-3"]', { state: 'attached', timeout: 6000 });
		await settle(page);
		const now = await rowTop(page, id);
		expect(Math.abs(now - top) <= 1, `row moved from ${top} to ${now}`);
	});
	await context.close();
}

// ---- a pass that adds a row above and drops one as tall below keeps the reading position ----
for (const size of [
	{ width: 721, height: 900 },
	{ width: 1280, height: 800 },
	{ width: 390, height: 844 }
]) {
	const { context, page } = await openFeed(size, '#/', { fixed: false, label: 'swap' });
	const seen = new Map();
	let first = false;
	let swap = null;
	await page.route('**/v1/rpc', async (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (op !== 'owner.feed') return route.fallback();
		const res = await route.fetch();
		const body = await res.json();
		for (const m of body.result?.messages ?? []) seen.set(m.message_id, m);
		const newest = !JSON.parse(route.request().postData()).body?.before;
		const top = [...seen.values()].reduce((a, b) => (a.seq > b.seq ? a : b));
		if (first && newest) body.result.messages.unshift({ ...top, message_id: 'ARRIVED-4', seq: top.seq + 500 });
		if (swap) {
			// the dropped row comes back above as a new message: the same text, names and place
			const gone = seen.get(swap);
			body.result.messages = body.result.messages.filter((m) => m.message_id !== swap);
			if (newest) body.result.messages.unshift({ ...gone, message_id: 'SWAPPED-IN', seq: top.seq + 1000, date: top.date, reply_to: null });
		}
		return route.fulfill({ response: res, body: JSON.stringify(body) });
	});
	await page.reload({ waitUntil: 'networkidle' });
	await page.waitForSelector('.col [data-id]');
	await check('swap: a row arriving above while one as tall leaves below keeps the row in view within 1 px', async () => {
		const id = await scrollRowToTop(page, 9);
		await settle(page);
		// a first arrival puts up the last-visit divider, so the swap itself keeps the height
		first = true;
		await pass(page);
		await page.waitForSelector('.col a.row[data-id="ARRIVED-4"]', { state: 'attached', timeout: 6000 });
		await settle(page);
		const top = await rowTop(page, id);
		// a row below the reading row with rows on both sides, so no heading comes or goes with it
		swap = await page.evaluate((id) => {
			const rows = [...document.querySelectorAll('.col a.row')];
			const at = rows.findIndex((r) => r.dataset.id === id);
			return rows.slice(at + 2).find((r) => r.previousElementSibling?.matches('a.row') && r.nextElementSibling?.matches('a.row'))?.dataset.id ?? null;
		}, id);
		expect(swap, 'no row to drop');
		const height = await page.evaluate(() => document.querySelector('.col .list').getBoundingClientRect().height);
		await pass(page);
		await page.waitForSelector('.col a.row[data-id="SWAPPED-IN"]', { state: 'attached', timeout: 6000 });
		await settle(page);
		const now = await rowTop(page, id);
		const after = await page.evaluate(() => document.querySelector('.col .list').getBoundingClientRect().height);
		expect(Math.abs(after - height) < 1, `the list changed height (${height} to ${after}); the case needs it unchanged`);
		expect(Math.abs(now - top) <= 1, `row moved from ${top} to ${now}`);
	});
	await context.close();
}

// ---- a smooth scroll reaches its target through an older page and through a refresh ----
{
	const { context, page } = await openFeed({ width: 1280, height: 800 }, '#/', { fixed: false, label: 'smooth' });
	let truncate = true;
	/** The older page waits for this, so it lands while the list is moving. */
	let release;
	const held = new Promise((r) => (release = r));
	await page.route('**/v1/rpc', async (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (op !== 'owner.feed') return route.fallback();
		const b = JSON.parse(route.request().postData()).body ?? {};
		if (truncate && b.before !== undefined && b.after === undefined) await held;
		const res = await route.fetch();
		const body = await res.json();
		// the first window is 24 rows with more below, so the list loads an older page
		if (truncate && b.after === undefined && b.before === undefined) {
			body.result.messages = body.result.messages.slice(0, 24);
			body.result.more = true;
		}
		return route.fulfill({ response: res, body: JSON.stringify(body) });
	});
	await page.reload({ waitUntil: 'networkidle' });
	await page.waitForSelector('.col [data-id]');
	await page.waitForTimeout(2200); // past the 2 s between refresh passes
	await page.exposeFunction('releaseOlder', () => release());
	/** Scrolls smoothly from `from` to `to`, starts `during` as it moves, answers where it stopped. */
	const glide = (from, to, during) =>
		page.evaluate(
			async ({ from, to, during }) => {
				const col = document.querySelector('.col');
				col.scrollTop = from;
				await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
				col.scrollTo({ top: to, behavior: 'smooth' });
				await new Promise((r) => setTimeout(r, 60));
				const moving = col.scrollTop > from && col.scrollTop < to;
				if (during === 'refresh') document.dispatchEvent(new Event('visibilitychange'));
				else await window.releaseOlder();
				await new Promise((r) => setTimeout(r, 2000));
				return { moving, at: col.scrollTop, rows: col.querySelectorAll('a.row').length };
			},
			{ from, to, during }
		);
	await check('smooth: a smooth scroll to the bottom reaches it while an older page is appended', async () => {
		const rows = await page.locator('.col a.row').count();
		const to = await page.evaluate(() => {
			const col = document.querySelector('.col');
			return col.scrollHeight - col.clientHeight;
		});
		const r = await glide(0, to, 'older');
		expect(r.rows > rows, `no older page was appended (${rows} rows)`);
		expect(r.moving, 'the scroll did not animate');
		expect(Math.abs(r.at - to) <= 1, `stopped at ${r.at} of ${to}`);
	});
	await check('smooth: a smooth scroll reaches its target through a refresh that changes nothing', async () => {
		truncate = false;
		const r = await glide(300, 1300, 'refresh');
		expect(r.moving, 'the scroll did not animate');
		expect(Math.abs(r.at - 1300) <= 1, `stopped at ${r.at}`);
	});
	await context.close();
}

// ---- widening from the phone to the desktop keeps the row that was being read ----
{
	// a desktop window narrowed to the phone's width and widened again
	where = 'widen 390';
	const context = await browser.newContext({ viewport: { width: 390, height: 844 }, locale: 'en-GB', timezoneId: 'Europe/Budapest', reducedMotion: 'reduce' });
	const page = await context.newPage();
	await page.clock.setFixedTime(NOW);
	await page.goto(FEED, { waitUntil: 'networkidle' });
	await page.waitForSelector('.col [data-id]');
	await settle(page);
	/** The row's distance below the top edge of the list (under the head where it sticks). */
	const offset = (id) =>
		page.evaluate((id) => {
			const col = document.querySelector('.col');
			const head = col.querySelector('.head');
			const edge = Math.max(col.getBoundingClientRect().top, getComputedStyle(head).position === 'sticky' ? head.getBoundingClientRect().bottom : -Infinity);
			return document.querySelector(`.col [data-id="${id}"]`).getBoundingClientRect().top - edge;
		}, id);
	await check('widen: from 390 to 1280 px the top row of a list read halfway down stays at its place under the head within 1 px', async () => {
		// the middle row 20 px under the top of the list (the head scrolls away on the phone)
		const id = await page.evaluate(() => {
			const col = document.querySelector('.col');
			const rows = col.querySelectorAll('a.row');
			const row = rows[Math.floor(rows.length / 2)];
			col.scrollTop += row.getBoundingClientRect().top - col.getBoundingClientRect().top - 20;
			return row.dataset.id;
		});
		await settle(page);
		const before = await offset(id);
		await page.setViewportSize({ width: 1280, height: 844 });
		await settle(page);
		const after = await offset(id);
		expect(Math.abs(after - before) <= 1, `row offset ${before} became ${after}`);
	});
	await context.close();
}

/** A desktop window at `width` on the Feed, with a row's offset under the list's top edge (under
 * the head where it sticks) to read and to set. */
async function resizable(width, label, fixed = true) {
	where = `${label} ${width}`;
	const context = await browser.newContext({ viewport: { width, height: 844 }, locale: 'en-GB', timezoneId: 'Europe/Budapest', reducedMotion: 'reduce' });
	const page = await context.newPage();
	if (fixed) await page.clock.setFixedTime(NOW);
	await page.goto(FEED, { waitUntil: 'networkidle' });
	await page.waitForSelector('.col [data-id]');
	await settle(page);
	const edgeOf = () => {
		const col = document.querySelector('.col');
		const head = col.querySelector('.head');
		return Math.max(col.getBoundingClientRect().top, getComputedStyle(head).position === 'sticky' ? head.getBoundingClientRect().bottom : -Infinity);
	};
	const offset = (id) =>
		page.evaluate(
			({ id, edgeOf }) => document.querySelector(`.col [data-id="${id}"]`).getBoundingClientRect().top - new Function(`return (${edgeOf})()`)(),
			{ id, edgeOf: edgeOf.toString() }
		);
	/** The middle row put `px` under the edge (the head moves with the scroll on the phone, so twice). */
	const middle = (px) =>
		page.evaluate(
			({ px, edgeOf }) => {
				const edge = new Function(`return (${edgeOf})()`);
				const col = document.querySelector('.col');
				const rows = col.querySelectorAll('a.row');
				const row = rows[Math.floor(rows.length / 2)];
				for (let i = 0; i < 3; i++) col.scrollTop += row.getBoundingClientRect().top - edge() - px;
				return row.dataset.id;
			},
			{ px, edgeOf: edgeOf.toString() }
		);
	return { context, page, offset, middle };
}

// ---- resizing there and back keeps the row being read, settled or in quick succession ----
{
	const { context, page, offset, middle } = await resizable(390, 'round trip');
	for (const quick of [false, true]) {
		await check(`round trip: 390 to 1280 and back to 390 px${quick ? ' in quick succession' : ''} leaves the row being read where it was within 1 px`, async () => {
			const id = await middle(100);
			await settle(page);
			const before = await offset(id);
			await page.setViewportSize({ width: 1280, height: 844 });
			if (!quick) await settle(page);
			await page.setViewportSize({ width: 390, height: 844 });
			await settle(page);
			const after = await offset(id);
			expect(Math.abs(after - before) <= 1, `row offset ${before} became ${after}`);
		});
	}
	await context.close();
}

/** The first row whose top is in view and its offset under the edge, as the reader sees it. */
const reading = (page) =>
	page.evaluate(() => {
		const col = document.querySelector('.col');
		const head = col.querySelector('.head');
		const edge = Math.max(col.getBoundingClientRect().top, getComputedStyle(head).position === 'sticky' ? head.getBoundingClientRect().bottom : -Infinity);
		for (const row of col.querySelectorAll('a.row')) {
			const top = row.getBoundingClientRect().top;
			if (top >= edge - 1) return { id: row.dataset.id, offset: top - edge };
		}
		return null;
	});

// ---- the held row leaving the list between two resizes ----
{
	const { context, page, offset, middle } = await resizable(390, 'held gone', false);
	let drop = null;
	await page.route('**/v1/rpc', async (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (op !== 'owner.feed' || !drop) return route.fallback();
		const res = await route.fetch();
		const body = await res.json();
		body.result.messages = body.result.messages.filter((m) => m.message_id !== drop);
		return route.fulfill({ response: res, body: JSON.stringify(body) });
	});
	await check('held gone: the row held across a resize leaves with a refresh; the next resize keeps the row in its place within 1 px', async () => {
		const id = await middle(32);
		await settle(page);
		await page.setViewportSize({ width: 1280, height: 844 });
		await settle(page);
		await page.waitForTimeout(2200); // past the 2 s between refresh passes
		drop = id;
		await pass(page);
		await page.waitForSelector(`.col a.row[data-id="${id}"]`, { state: 'detached', timeout: 6000 });
		await settle(page);
		const r = await reading(page);
		await page.setViewportSize({ width: 390, height: 844 });
		await settle(page);
		const after = await offset(r.id);
		expect(Math.abs(after - r.offset) <= 1, `row offset ${r.offset} became ${after}`);
	});
	await context.close();
}

// ---- the reader's scroll cancelled out by the browser's anchoring between two resizes ----
{
	const { context, page, offset, middle } = await resizable(390, 'input', false);
	let arrive = false;
	await page.route('**/v1/rpc', async (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (op !== 'owner.feed' || !arrive) return route.fallback();
		const res = await route.fetch();
		const body = await res.json();
		const top = body.result?.messages?.[0];
		if (!JSON.parse(route.request().postData()).body?.before && top) body.result.messages.unshift({ ...top, message_id: 'ARRIVED-6', seq: top.seq + 3000 });
		return route.fulfill({ response: res, body: JSON.stringify(body) });
	});
	await check('input: a wheel up as tall as an arrival above leaves no net scroll; the next resize keeps the row in its place within 1 px', async () => {
		await middle(32);
		await settle(page);
		await page.setViewportSize({ width: 1280, height: 844 });
		await settle(page);
		await page.waitForTimeout(2200);
		// as the arrival goes in, the reader wheels up by its height: the browser's adjustment
		// for the arrival and the reader's scroll cancel out in one frame
		await page.evaluate(() => {
			const col = document.querySelector('.col');
			const list = col.querySelector('.list');
			const was = list.getBoundingClientRect().height;
			const mo = new MutationObserver(() => {
				if (!list.querySelector('a.row[data-id="ARRIVED-6"]')) return;
				mo.disconnect();
				const by = list.getBoundingClientRect().height - was;
				col.dispatchEvent(new WheelEvent('wheel', { deltaY: -by, bubbles: true }));
				col.scrollTop -= by;
			});
			mo.observe(list, { childList: true, subtree: true });
		});
		arrive = true;
		await pass(page);
		await page.waitForSelector('.col a.row[data-id="ARRIVED-6"]', { state: 'attached', timeout: 6000 });
		await settle(page);
		const r = await reading(page);
		await page.setViewportSize({ width: 390, height: 844 });
		await settle(page);
		const after = await offset(r.id);
		expect(Math.abs(after - r.offset) <= 1, `row offset ${r.offset} became ${after}`);
	});
	await context.close();
}

// ---- rows that land while the width changes keep the row being read in place ----
for (const [from, to] of [
	[1280, 390],
	[720, 721]
]) {
	const { context, page, offset, middle } = await resizable(from, 'resize and rows', false);
	let arrive = false;
	let release;
	let asked;
	const requested = new Promise((r) => (asked = r));
	const held = new Promise((r) => (release = r));
	await page.route('**/v1/rpc', async (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (op !== 'owner.feed' || !arrive) return route.fallback();
		const newest = !JSON.parse(route.request().postData()).body?.before;
		if (newest) {
			asked();
			await held;
		}
		const res = await route.fetch();
		const body = await res.json();
		const top = body.result?.messages?.[0];
		if (newest && top) body.result.messages.unshift({ ...top, message_id: 'ARRIVED-5', seq: top.seq + 2000, text: 'Arrived as the window was resized; long enough to take two lines on the phone.' });
		return route.fulfill({ response: res, body: JSON.stringify(body) });
	});
	await check(`resize and rows: ${from} to ${to} px with an arrival landing at once keeps the row being read within 1 px`, async () => {
		const id = await middle(20);
		await page.waitForTimeout(2200); // past the 2 s between refresh passes
		const before = await offset(id);
		arrive = true;
		await pass(page);
		await requested;
		await Promise.all([page.setViewportSize({ width: to, height: 844 }), release()]);
		await page.waitForSelector('.col a.row[data-id="ARRIVED-5"]', { state: 'attached', timeout: 6000 });
		await settle(page);
		const after = await offset(id);
		expect(Math.abs(after - before) <= 1, `row offset ${before} became ${after}`);
	});
	await context.close();
}

// ---- a long place name never runs into the text of a wide row ----
for (const width of [1280, 1600, 1920]) {
	const { context, page } = await openFeed({ width, height: 900 }, '#/', { label: 'place' });
	await check('place: the place and the delivery word stay clear of the text, a long channel name too', async () => {
		const r = await page.evaluate(() => {
			/** The painted extent of an element's text, clipped to its box when it clips. */
			const ink = (el) => {
				const range = document.createRange();
				range.selectNodeContents(el);
				const t = range.getBoundingClientRect();
				const b = el.getBoundingClientRect();
				return getComputedStyle(el).overflowX === 'hidden' ? { left: Math.max(t.left, b.left), right: Math.min(t.right, b.right), top: b.top, bottom: b.bottom } : t;
			};
			const hit = (a, b) => a.left < b.right - 0.5 && a.right > b.left + 0.5 && a.top < b.bottom - 0.5 && a.bottom > b.top + 0.5;
			const rows = [...document.querySelectorAll('.col a.row')];
			const bad = rows.filter((row) => {
				const text = row.querySelector('.text').getBoundingClientRect();
				return [row.querySelector('.place'), row.querySelector('.meta')].some((el) => el && hit(ink(el), text));
			});
			return { long: rows.some((row) => row.querySelector('.place')?.textContent === '#infrastructure-reviews'), bad: bad.map((row) => row.querySelector('.place')?.textContent) };
		});
		expect(r.long, 'no row in #infrastructure-reviews');
		expect(r.bad.length === 0, `over the text: ${r.bad.join(', ')}`);
	});
	await context.close();
}

// ---- the sheet's history when the reader leaves for the conversation and comes back ----
for (const size of [
	{ width: 390, height: 844 },
	{ width: 1180, height: 820 }
]) {
	const { context, page } = await openFeed(size, '#/', { label: 'sheet' });
	await check('sheet: to the conversation, Back restores the sheet, Forward returns to the conversation, Back twice closes it', async () => {
		const id = await scrollRowToTop(page, 3);
		await page.locator(`.col [data-id="${id}"]`).click();
		await page.waitForSelector('.feedpage dialog .thread .acts a');
		await settle(page);
		await page.locator('.feedpage dialog .thread .acts a').first().click();
		await page.waitForSelector('.msg');
		const convo = await hashOf(page);
		expect(convo.includes(`/m/${id}`), `not the conversation at the message: ${convo}`);
		await page.goBack();
		await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect((await hashOf(page)) === `#/t/${id}`, await hashOf(page));
		await page.goForward();
		await settle(page);
		expect((await hashOf(page)) === convo, `Forward stayed at ${await hashOf(page)}`);
		await page.goBack();
		await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		await page.goBack();
		await page.waitForFunction(() => !document.querySelector('.feedpage dialog')?.open);
		await settle(page);
		expect((await hashOf(page)) === '#/', `the second Back left ${await hashOf(page)}`);
	});
	if (size.width < 720) {
		await check('sheet by touch: the handle toggles full height', async () => {
			await page.locator('.col a.row').nth(2).tap();
			await page.waitForFunction(() => document.querySelector('.feedpage dialog')?.open);
			const handle = page.locator('.feedpage dialog .handle');
			await handle.tap();
			let b = await box(page, '.feedpage dialog');
			expect(Math.round(b.height) === 844 && (await handle.getAttribute('aria-expanded')) === 'true', JSON.stringify(b));
			await handle.tap();
			b = await box(page, '.feedpage dialog');
			expect(b.height < 844 * 0.8 && (await handle.getAttribute('aria-expanded')) === 'false', JSON.stringify(b));
		});
	}
	await context.close();
}

// ---- Agents and Channels keep their rows when a refresh fails, and say so ----
for (const hash of ['#/agents', '#/channels']) {
	const { context, page } = await openFeed({ width: 1440, height: 900 }, hash, { fixed: false, label: hash.slice(2), wait: '.index li' });
	const failing = new Set();
	await page.route('**/v1/rpc', (route) => {
		const op = JSON.parse(route.request().postData() ?? '{}').op;
		if (failing.has(op)) return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: false, error: { code: 'unavailable', message: `${op} unavailable` } }) });
		return route.fallback();
	});
	const rows = await page.locator('.index li').count();
	await check('a failed owner.list keeps the rows and shows the error line with its age and Retry; Retry recovers', async () => {
		failing.add('owner.list');
		await pass(page);
		const alert = page.getByRole('alert').filter({ hasText: 'owner.list' });
		await alert.waitFor({ timeout: 6000 });
		expect(/as of \d\d:\d\d/.test(await alert.textContent()), `no age: ${await alert.textContent()}`);
		expect((await page.locator('.index li').count()) === rows, 'the rows did not stay');
		failing.delete('owner.list');
		await alert.getByRole('button', { name: 'Retry' }).click();
		await alert.waitFor({ state: 'detached', timeout: 6000 });
	});
	if (hash === '#/agents') {
		await check('a failed fleet.list shows its own error line; no agent reads "Not on any host yet"', async () => {
			failing.add('fleet.list');
			await pass(page);
			const alert = page.getByRole('alert').filter({ hasText: 'fleet.list' });
			await alert.waitFor({ timeout: 6000 });
			expect((await page.locator('.index li').count()) === rows, 'the rows did not stay');
			expect(!(await page.locator('.index').textContent()).includes('Not on any host yet'), 'reads "Not on any host yet"');
			failing.delete('fleet.list');
		});
	}
	await context.close();
}

// ---- the keys page (a server page; the mock serves its template) fits the phone ----
{
	where = 'keys 390';
	const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
	const page = await context.newPage();
	await check('keys at 390: no horizontal overflow', async () => {
		await page.goto(`${FEED}keys`, { waitUntil: 'networkidle' });
		const w = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
		expect(w[0] <= w[1], `scrollWidth ${w[0]} > clientWidth ${w[1]}`);
		const main = await page.evaluate(() => [document.querySelector('.main').scrollWidth, document.querySelector('.main').clientWidth]);
		expect(main[0] <= main[1], `main scrollWidth ${main[0]} > clientWidth ${main[1]}`);
	});
	await check('keys at 390: the app’s top bar and tab bar, in the Feed page’s place', async () => {
		const bars = () =>
			page.evaluate(() =>
				['.mbar', '.tabs', '.mword', '.tabs > :last-child'].map((q) => {
					const b = document.querySelector(q).getBoundingClientRect();
					return `${q} ${Math.round(b.x)},${Math.round(b.y)} ${Math.round(b.width)}x${Math.round(b.height)}`;
				})
			);
		const keys = await bars();
		await page.goto(FEED, { waitUntil: 'networkidle' });
		await page.waitForSelector('.tabs .who');
		const feed = await bars();
		expect(keys.join() === feed.join(), `keys ${keys.join(' | ')} feed ${feed.join(' | ')}`);
		expect((await page.locator('.mast').isVisible()) === false, 'the masthead shows at 390');
	});
	await context.close();
}

// ---- the keys page sits in the app's shell: the same masthead in the same place, Keys current ----
{
	where = 'keys shell 1440';
	const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
	const page = await context.newPage();
	const csp = [];
	page.on('console', (m) => m.type() === 'error' && /Content Security Policy/.test(m.text()) && csp.push(m.text()));
	/** The masthead's parts and their boxes, read in the browser. */
	const mast = () =>
		page.evaluate(() => {
			const r = (el) => (el ? [...['x', 'y', 'width', 'height'].map((k) => Math.round(el.getBoundingClientRect()[k]))] : null);
			const m = document.querySelector('.mast');
			return {
				mast: r(m),
				word: r(m.querySelector('.word')),
				kick: r(m.querySelector('.kick')),
				sections: [...m.querySelectorAll('.sections a')].map((a) => [a.textContent, ...r(a)]),
				lists: [...m.querySelectorAll('ul')].map((u) => [[...u.querySelectorAll('li')].map((li) => li.textContent.replace(/\s+/g, ' ').trim()).join(' | '), ...r(u)]),
				scope: [m.querySelector('.scope').textContent, ...r(m.querySelector('.scope'))],
				who: [m.querySelector('.who').textContent.trim(), ...r(m.querySelector('.who'))],
				font: getComputedStyle(m.querySelector('.word')).fontFamily,
				loaded: [...document.fonts].filter((f) => f.status === 'loaded').map((f) => `${f.family} ${f.weight} ${f.style}`).sort()
			};
		});
	await page.goto(FEED, { waitUntil: 'networkidle' });
	await page.waitForSelector('.mast li a');
	const feed = await mast();
	await page.goto(`${FEED}keys`, { waitUntil: 'networkidle' });
	await page.evaluate(() => document.fonts.ready);
	const keys = await mast();
	await check('keys: the masthead has the Feed page’s parts, text and boxes; only the current section differs', async () => {
		for (const k of ['mast', 'word', 'kick', 'sections', 'lists', 'scope', 'who'])
			expect(JSON.stringify(feed[k]) === JSON.stringify(keys[k]), `${k}: feed ${JSON.stringify(feed[k])} keys ${JSON.stringify(keys[k])}`);
		const current = await page.locator('.mast .sections a[aria-current=page]').textContent();
		expect(current === 'Keys', current);
	});
	await check('keys: the app’s fonts load under the page’s CSP', async () => {
		expect(keys.font === feed.font && keys.loaded.includes('Source Serif 4 600 normal') && keys.loaded.includes('Inter 400 normal'), JSON.stringify(keys.loaded));
		expect(csp.length === 0, `CSP violations: ${csp.join(', ')}`);
	});
	await check('keys: the user menu opens upward in the foot; Light applies and is stored; Escape closes it', async () => {
		await page.locator('.mast .who').click();
		const panel = page.locator('.mast [role=menu]');
		expect(await panel.isVisible(), 'the menu did not open');
		const items = await panel.locator('[data-theme-choice]').evaluateAll((els) => els.map((e) => `${e.textContent.trim()}:${e.getAttribute('aria-checked')}`));
		expect(items.join(' ') === 'System:true Light:false Dark:false', items.join(' '));
		await panel.locator('[data-theme-choice=light]').click();
		const s = await page.evaluate(() => [document.documentElement.dataset.theme, getComputedStyle(document.body).backgroundColor, localStorage.getItem('conde.theme')]);
		expect(s.join() === 'light,rgb(246, 243, 237),light', s.join());
		expect((await panel.locator('[data-theme-choice=light]').getAttribute('aria-checked')) === 'true', 'Light not checked');
		await page.keyboard.press('Escape');
		expect(!(await panel.isVisible()), 'Escape did not close the menu');
		await page.locator('.mast .who').click();
		await page.mouse.click(900, 400);
		expect(!(await panel.isVisible()), 'a click outside did not close the menu');
		await page.evaluate(() => localStorage.removeItem('conde.theme'));
	});
	await context.close();
}

// ---- the agent page's list: long names wrap; the handle resizes it, kept per browser ----
{
	const LONG = 'david/infrastructure-cost-report-reviewer';
	const { context, page } = await openFeed({ width: 1440, height: 900 }, `#/a/desk/dm/${LONG}`, { label: 'agent list', wait: '.msg' });
	const listWidth = () => page.evaluate(() => Math.round(document.querySelector('.agent-page .list').getBoundingClientRect().width));
	const stored = () => page.evaluate(() => localStorage.getItem('conde.agentlist.width'));
	const handle = page.locator('.agent-page [role=separator]');
	await check('agent list: a long name wraps onto a second line, uncut, its time on the first line', async () => {
		const r = await page.evaluate((long) => {
			const a = [...document.querySelectorAll('.agent-page .list a')].find((x) => x.textContent.includes(long));
			const span = a.querySelector('span');
			const time = a.querySelector('time');
			const line = parseFloat(getComputedStyle(span).lineHeight);
			return {
				text: span.textContent,
				lines: Math.round(span.getBoundingClientRect().height / line),
				cut: span.scrollWidth > span.clientWidth || getComputedStyle(span).textOverflow === 'ellipsis',
				first: Math.abs(time.getBoundingClientRect().bottom - span.getBoundingClientRect().top - line) < line / 2 + 2,
				timeFits: time.scrollWidth <= time.clientWidth + 1
			};
		}, LONG);
		expect(r.text === LONG && r.lines >= 2 && !r.cut, JSON.stringify(r));
		expect(r.first && r.timeFits, `time not on the first line or cut: ${JSON.stringify(r)}`);
	});
	await check('agent list: the default is 290 px; dragging the handle stops at 200 and 640 px', async () => {
		expect((await listWidth()) === 290, `${await listWidth()}`);
		const b = await box(page, '.agent-page [role=separator]');
		const y = b.y + 300;
		const cx = b.x + b.width / 2;
		await page.mouse.move(cx, y);
		await page.mouse.down();
		await page.mouse.move(cx - 60, y, { steps: 4 });
		expect((await listWidth()) === 230, `mid-drag ${await listWidth()}`);
		await page.mouse.move(cx - 400, y, { steps: 4 });
		await page.mouse.up();
		expect((await listWidth()) === 200 && (await stored()) === '200', `narrow: ${await listWidth()}, stored ${await stored()}`);
		const c = await box(page, '.agent-page [role=separator]');
		await page.mouse.move(c.x + c.width / 2, y);
		await page.mouse.down();
		await page.mouse.move(c.x + 700, y, { steps: 6 });
		await page.mouse.up();
		expect((await listWidth()) === 640 && (await stored()) === '640', `wide: ${await listWidth()}, stored ${await stored()}`);
	});
	await check('agent list: the width survives a reload', async () => {
		await page.reload({ waitUntil: 'networkidle' });
		await page.waitForSelector('.msg');
		expect((await listWidth()) === 640, `${await listWidth()}`);
	});
	await check('agent list: the handle is a focusable separator; arrows resize, Home goes to 200, aria-valuenow follows', async () => {
		await handle.focus();
		const f = await focus(page);
		expect(f.role === 'separator', JSON.stringify(f));
		await page.keyboard.press('ArrowLeft');
		expect((await listWidth()) === 624 && (await handle.getAttribute('aria-valuenow')) === '624', `${await listWidth()} / ${await handle.getAttribute('aria-valuenow')}`);
		await page.keyboard.press('Shift+ArrowLeft');
		expect((await listWidth()) === 576, `${await listWidth()}`);
		await page.keyboard.press('ArrowRight');
		expect((await listWidth()) === 592 && (await stored()) === '592', `${await listWidth()}, stored ${await stored()}`);
		await page.keyboard.press('Home');
		expect((await listWidth()) === 200, `${await listWidth()}`);
		await page.keyboard.press('ArrowLeft');
		expect((await listWidth()) === 200, 'went below 200');
	});
	await check('agent list: a double-click on the handle resets the default and forgets the width', async () => {
		await handle.dblclick();
		expect((await listWidth()) === 290 && (await stored()) === null, `${await listWidth()}, stored ${await stored()}`);
	});
	await context.close();
	const phone = await openFeed({ width: 390, height: 844 }, '#/a/desk', { label: 'agent list', wait: '.agent-page .list a' });
	await check('agent list on the phone: no handle, the list full width', async () => {
		expect(!(await phone.page.locator('.agent-page [role=separator]').isVisible()), 'handle visible');
		const w = await phone.page.evaluate(() => [document.querySelector('.agent-page .list').getBoundingClientRect().width, document.documentElement.scrollWidth]);
		expect(w[0] === 390 && w[1] <= 390, JSON.stringify(w));
	});
	await phone.context.close();
}

// ---- the agent page's push setting per channel: a bell beside each channel, the board's answer ----
{
	const bell = (page, ch) => page.getByRole('button', { name: `Notify desk of every post in ${ch}` });
	const pressed = async (page, ch) => (await bell(page, ch).getAttribute('aria-pressed')) === 'true';
	const { context, page } = await openFeed({ width: 1440, height: 900 }, '#/a/desk', { label: 'notify', wait: '.agent-page .ch' });
	const sent = [];
	page.on('request', (r) => {
		const body = r.url().endsWith('/v1/rpc') ? JSON.parse(r.postData() ?? '{}') : {};
		if (body.op === 'owner.notify') sent.push(body);
	});
	await check('notify: each channel row has a bell beside its link; pressed for every post, not for mentions', async () => {
		const rows = await page.locator('.agent-page .ch').evaluateAll((els) =>
			els.map((e) => [e.querySelector('a span').textContent, !!e.querySelector('a button'), e.querySelector('button.notify')?.getAttribute('aria-pressed')])
		);
		expect(rows.length === 3 && rows.every((r) => !r[1]), `rows ${JSON.stringify(rows)}`);
		expect((await pressed(page, '#conde-dev')) && !(await pressed(page, '#kudos')) && !(await pressed(page, '#lobby')), JSON.stringify(rows));
	});
	await check('notify: Tab from a channel link reaches its bell; Enter turns every post on with an idempotency key', async () => {
		await page.locator('.agent-page .ch a', { hasText: '#kudos' }).focus();
		await page.keyboard.press('Tab');
		const f = await focus(page);
		expect(f.tag === 'button' && (await bell(page, '#kudos').evaluate((b) => b === document.activeElement)), JSON.stringify(f));
		await page.keyboard.press('Enter');
		await settle(page);
		expect(await pressed(page, '#kudos'), 'not pressed after Enter');
		const last = sent.at(-1);
		expect(sent.length === 1 && last.body.agent === 'kdobmayer/desk' && last.body.channel === '#kudos' && last.body.notify === 'all', JSON.stringify(sent));
		expect(typeof last.idempotency_key === 'string' && last.idempotency_key.length > 0, 'no idempotency key');
	});
	await check('notify: the setting is the board’s: it survives a reload; Space turns it back to mentions', async () => {
		await page.reload({ waitUntil: 'networkidle' });
		await page.waitForSelector('.agent-page .ch');
		expect(await pressed(page, '#kudos'), 'lost on reload');
		await bell(page, '#kudos').focus();
		await page.keyboard.press('Space');
		await settle(page);
		expect(!(await pressed(page, '#kudos')), 'still pressed after Space');
		await page.reload({ waitUntil: 'networkidle' });
		await page.waitForSelector('.agent-page .ch');
		expect(!(await pressed(page, '#kudos')), 'pressed again after reload');
	});
	await check('notify: a refused change leaves the bell as it was and says so; Retry applies it', async () => {
		await page.route('**/v1/rpc', async (route) => {
			const body = JSON.parse(route.request().postData() ?? '{}');
			if (body.op !== 'owner.notify') return route.fallback();
			await route.fulfill({ json: { ok: false, error: { code: 'internal', message: 'the store did not answer' } } });
		});
		await bell(page, '#lobby').click();
		await settle(page);
		const alert = (await page.locator('.agent-page .notify-error [role=alert]').textContent()) ?? '';
		expect(!(await pressed(page, '#lobby')), 'pressed after a refusal');
		expect(/Could not change #lobby: the store did not answer\./.test(alert), alert);
		await page.unroute('**/v1/rpc');
		await page.locator('.agent-page .notify-error button', { hasText: 'Retry' }).click();
		await settle(page);
		expect((await pressed(page, '#lobby')) && (await page.locator('.agent-page .notify-error').count()) === 0, 'retry did not apply');
		await bell(page, '#lobby').click();
		await settle(page);
		expect(!(await pressed(page, '#lobby')), 'not reset');
	});
	await check('notify: a refused change stays with its agent: no Retry on another agent’s page; back on it, Retry changes that agent', async () => {
		await page.route('**/v1/rpc', async (route) => {
			const body = JSON.parse(route.request().postData() ?? '{}');
			if (body.op !== 'owner.notify') return route.fallback();
			await route.fulfill({ json: { ok: false, error: { code: 'internal', message: 'the store did not answer' } } });
		});
		await bell(page, '#lobby').click();
		await settle(page);
		await page.unroute('**/v1/rpc');
		expect((await page.locator('.agent-page .notify-error').count()) === 1, 'no error line');
		const before = sent.length;
		// by the rail's links, as a person goes: the agent page's layout stays mounted
		await page.locator('a[href="#/a/dakai-helper"]').first().click();
		await page.locator('.agent-page .list h1', { hasText: 'dakai-helper' }).waitFor();
		await settle(page);
		expect((await page.locator('.agent-page .notify-error').count()) === 0, 'desk’s failure shows on dakai-helper’s page');
		await page.locator('a[href="#/a/desk"]').first().click();
		await page.locator('.agent-page .list h1', { hasText: /^desk$/ }).waitFor();
		await page.locator('.agent-page .notify-error button', { hasText: 'Retry' }).click();
		await settle(page);
		const retried = sent.slice(before).map((r) => [r.body.agent, r.body.channel, r.body.notify]);
		expect(JSON.stringify(retried) === JSON.stringify([['kdobmayer/desk', '#lobby', 'all']]), JSON.stringify(retried));
		expect(await pressed(page, '#lobby'), 'retry did not apply');
		await bell(page, '#lobby').click();
		await settle(page);
		expect(!(await pressed(page, '#lobby')), 'not reset');
	});
	await check('notify: a rail read sent before a later change never lands over it', async () => {
		// the first change's rail read is held until the second change's read has landed
		let release = null;
		let reads = 0;
		await page.route('**/v1/rpc', async (route) => {
			const body = JSON.parse(route.request().postData() ?? '{}');
			if (body.op !== 'owner.list') return route.fallback();
			const response = await route.fetch();
			if (++reads === 1) await new Promise((resolve) => (release = resolve));
			await route.fulfill({ response });
		});
		await bell(page, '#kudos').click();
		for (let i = 0; i < 100 && !release; i++) await page.waitForTimeout(20);
		expect(release, 'no rail read after the first change');
		await bell(page, '#kudos').click();
		await settle(page);
		expect(!(await pressed(page, '#kudos')), 'pressed after the second change');
		release();
		await settle(page);
		expect(!(await pressed(page, '#kudos')), 'the first change’s read put every post back');
		await page.unroute('**/v1/rpc');
		await page.reload({ waitUntil: 'networkidle' });
		await page.waitForSelector('.agent-page .ch');
		expect(!(await pressed(page, '#kudos')), 'the board holds every post');
	});
	await context.close();
	const phone = await openFeed({ width: 390, height: 844 }, '#/a/desk', { label: 'notify', wait: '.agent-page .ch' });
	await check('notify on the phone: a 44 px target that a tap toggles, no sideways scroll', async () => {
		const b = await box(phone.page, '.agent-page .ch:has(a[href="#/c/kudos"]) button.notify');
		expect(b.width >= 44 && b.height >= 44 && b.x + b.width <= 390, JSON.stringify(b));
		await bell(phone.page, '#kudos').tap();
		await settle(phone.page);
		expect(await pressed(phone.page, '#kudos'), 'tap did not turn it on');
		await bell(phone.page, '#kudos').tap();
		await settle(phone.page);
		expect(!(await pressed(phone.page, '#kudos')), 'tap did not turn it off');
		const w = await phone.page.evaluate(() => document.documentElement.scrollWidth);
		expect(w <= 390, `scroll width ${w}`);
	});
	await phone.context.close();
}

// ---- message bodies render the markdown subset; previews stay plain ----
{
	const id = await import('../../src/lib/design/fixture.ts').then((fx) => fx.ids.markdown);
	const { context, page } = await openFeed({ width: 1440, height: 900 }, `#/t/${id}`, { label: 'markdown', wait: 'aside.side .thread ol li .md' });
	const body = page.locator(`aside.side li[data-id="${id}"] .md`);
	await check('markdown: the thread shows bold, a list, inline code, a code box that scrolls sideways, a quote and a safe link', async () => {
		const r = await body.evaluate((md) => {
			const pre = md.querySelector('pre');
			const a = md.querySelector('a');
			return {
				strong: md.querySelector('strong')?.textContent,
				items: md.querySelectorAll('ol > li').length,
				code: md.querySelector('li code')?.textContent,
				pre: pre && { scrolls: pre.scrollWidth > pre.clientWidth, overflow: getComputedStyle(pre).overflowX, font: getComputedStyle(pre.querySelector('code')).fontFamily },
				quote: md.querySelector('blockquote')?.textContent.trim(),
				link: a && { href: a.getAttribute('href'), target: a.target, rel: a.rel },
				heading: md.querySelectorAll('h1, h2, h3, h4, h5, h6, img, table, script').length,
				name: md.textContent.includes('kdobmayer/desk:')
			};
		});
		expect(r.strong === 'October cost report' && r.items === 3 && r.code === 'nightly-e2e' && r.name, JSON.stringify(r));
		expect(r.pre?.scrolls && r.pre.overflow === 'auto' && /monospace/.test(r.pre.font), `code box: ${JSON.stringify(r.pre)}`);
		expect(r.quote === 'Numbers are from the billing export, not estimates.', `quote: ${r.quote}`);
		expect(r.link?.href === 'https://example.com/reports/2026-10' && r.link.target === '_blank' && /noopener/.test(r.link.rel), `link: ${JSON.stringify(r.link)}`);
		expect(r.heading === 0, 'a heading, image, table or script element');
		const csp = await page.evaluate(() => window.__csp);
		expect(csp.length === 0, `CSP violations: ${csp.join(', ')}`);
	});
	await check('markdown: the Feed row and the thread headline are plain text, no markup characters', async () => {
		const row = (await page.locator(`.col a.row[data-id="${id}"] .text`).textContent()).replace(/\s+/g, ' ').trim();
		expect(row.startsWith('October cost report is ready. Three items need a decision from kdobmayer/desk: 1. Move the nightly-e2e runner'), row.slice(0, 120));
		expect(!/[*`>]|```/.test(row), `markup in the row: ${row}`);
		const title = (await page.locator('aside.side .title').textContent()).trim();
		expect(title === 'October cost report is ready.', title);
	});
	await check('markdown: the conversation renders the same body; a plain message stays one paragraph', async () => {
		await page.goto(`${FEED}#/a/desk/dm/david/infrastructure-cost-report-reviewer`, { waitUntil: 'networkidle' });
		await page.waitForSelector('.msg .md pre');
		const plain = await page.locator('.msg').last().locator('.md').evaluate((md) => md.innerHTML);
		expect(plain === '<p>Agreed on 1 and 2. The GPU pool stays until #conde-dev has the November numbers.</p>', plain);
	});
	await context.close();
}

// ---- the theme: system by default, a choice in the user menu, kept per browser ----
{
	where = 'theme 1440';
	const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
	const page = await context.newPage();
	const state = () =>
		page.evaluate(() => ({
			theme: document.documentElement.dataset.theme,
			bg: getComputedStyle(document.body).backgroundColor,
			stored: localStorage.getItem('conde.theme')
		}));
	const LIGHT = 'rgb(246, 243, 237)';
	const DARK = 'rgb(19, 21, 24)';
	await check('theme: the inline script runs in the head before any stylesheet', async () => {
		const html = await (await page.request.get(FEED)).text();
		const script = html.indexOf("localStorage.getItem('conde.theme')");
		const sheet = html.search(/<link[^>]+rel="stylesheet"/);
		expect(script > 0 && sheet > script && script < html.indexOf('</head>'), `script at ${script}, stylesheet at ${sheet}`);
	});
	await check('theme: system follows the browser, also when it changes', async () => {
		await page.goto(FEED, { waitUntil: 'networkidle' });
		let s = await state();
		expect(s.theme === 'dark' && s.bg === DARK, JSON.stringify(s));
		await page.emulateMedia({ colorScheme: 'light' });
		await page.waitForFunction(() => document.documentElement.dataset.theme === 'light', null, { timeout: 2000 }).catch(() => {});
		s = await state();
		expect(s.theme === 'light' && s.bg === LIGHT, JSON.stringify(s));
		await page.emulateMedia({ colorScheme: 'dark' });
		await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark', null, { timeout: 2000 }).catch(() => {});
	});
	await check('theme: the user menu offers System, Light and Dark; Light applies at once and is stored', async () => {
		await page.locator('.mast .who').click();
		const items = await page.locator('[role=menu] [data-theme-choice]').evaluateAll((els) => els.map((e) => `${e.textContent.trim()}:${e.getAttribute('aria-checked')}`));
		expect(items.join(' ') === 'System:true Light:false Dark:false', items.join(' '));
		await page.locator('[data-theme-choice=light]').click();
		const s = await state();
		expect(s.theme === 'light' && s.bg === LIGHT && s.stored === 'light', JSON.stringify(s));
		expect((await page.locator('[data-theme-choice=light]').getAttribute('aria-checked')) === 'true', 'Light not checked');
	});
	await check('theme: a reload keeps the choice against the system, before the app starts', async () => {
		await page.addInitScript(() =>
			document.addEventListener('readystatechange', () => document.readyState === 'interactive' && (window.__themeAtParse = document.documentElement.dataset.theme), { once: true })
		);
		await page.reload({ waitUntil: 'networkidle' });
		const s = await state();
		const atParse = await page.evaluate(() => window.__themeAtParse);
		expect(atParse === 'light' && s.theme === 'light' && s.bg === LIGHT, JSON.stringify({ atParse, ...s }));
	});
	for (const [name, path] of [
		['keys', 'keys'],
		['join', 'join/mock']
	])
		await check(`theme: the ${name} page follows the stored choice`, async () => {
			await page.goto(`${FEED}${path}`, { waitUntil: 'networkidle' });
			const s = await state();
			expect(s.theme === 'light' && s.bg === LIGHT, JSON.stringify(s));
		});
	await check('theme: Dark overrides a light system; System clears the choice', async () => {
		await page.emulateMedia({ colorScheme: 'light' });
		await page.goto(FEED, { waitUntil: 'networkidle' });
		await page.locator('.mast .who').click();
		await page.locator('[data-theme-choice=dark]').click();
		let s = await state();
		expect(s.theme === 'dark' && s.bg === DARK && s.stored === 'dark', JSON.stringify(s));
		await page.locator('[data-theme-choice=system]').click();
		s = await state();
		expect(s.theme === 'light' && s.bg === LIGHT && s.stored === null, JSON.stringify(s));
	});
	await context.close();
}

// ---- the look: current by default, the beta in the user menu, kept per browser ----
{
	where = 'look 1440';
	const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
	const page = await context.newPage();
	const state = () =>
		page.evaluate(() => {
			const color = (sel, prop) => {
				const el = document.querySelector(sel);
				return el ? getComputedStyle(el)[prop] : null;
			};
			return {
				ui: document.documentElement.dataset.ui ?? null,
				theme: document.documentElement.dataset.theme,
				bg: getComputedStyle(document.body).backgroundColor,
				rail: color('.mast', 'backgroundColor'),
				held: color('.mast li b', 'color'),
				stored: localStorage.getItem('conde.ui')
			};
		});
	const DARK = 'rgb(19, 21, 24)';
	const BETA = 'rgb(19, 21, 23)';
	const RAIL = 'rgb(11, 25, 20)';
	const LIGHT = 'rgb(246, 243, 237)';
	await check('look: current by default; attention keeps today’s tone', async () => {
		await page.goto(FEED, { waitUntil: 'networkidle' });
		await page.waitForSelector('.mast li b');
		const s = await state();
		expect(s.ui === null && s.bg === DARK && s.held === 'rgb(212, 136, 107)' && s.stored === null, JSON.stringify(s));
	});
	await check('look: the user menu offers Current and Beta; Beta applies at once, is stored and says it is dark only', async () => {
		await page.locator('.mast .who').click();
		const items = await page.locator('[role=menu] [data-ui-choice]').evaluateAll((els) => els.map((e) => `${e.textContent.trim()}:${e.getAttribute('aria-checked')}`));
		expect(items.join(' ') === 'Current:true Beta:false', items.join(' '));
		expect(!(await page.locator('[role=menu] .note').isVisible()), 'the note shows for Current');
		await page.locator('[data-ui-choice=beta]').click();
		const s = await state();
		expect(s.ui === 'beta' && s.bg === BETA && s.rail === RAIL && s.held === 'rgb(223, 182, 123)' && s.stored === 'beta', JSON.stringify(s));
		expect((await page.locator('[data-ui-choice=beta]').getAttribute('aria-checked')) === 'true', 'Beta not checked');
		expect(await page.locator('[role=menu] .note').isVisible(), 'no note for Beta');
	});
	await check('look: a reload keeps the beta before the app starts', async () => {
		await page.addInitScript(() =>
			document.addEventListener('readystatechange', () => document.readyState === 'interactive' && (window.__uiAtParse = document.documentElement.dataset.ui), { once: true })
		);
		await page.reload({ waitUntil: 'networkidle' });
		const s = await state();
		const atParse = await page.evaluate(() => window.__uiAtParse);
		expect(atParse === 'beta' && s.ui === 'beta' && s.bg === BETA, JSON.stringify({ atParse, ...s }));
	});
	await check('look: the keys page follows the beta and its menu shows it', async () => {
		await page.goto(`${FEED}keys`, { waitUntil: 'networkidle' });
		const s = await state();
		expect(s.ui === 'beta' && s.bg === BETA && s.rail === RAIL, JSON.stringify(s));
		await page.locator('.mast .who').click();
		const panel = page.locator('.mast [role=menu]');
		const items = await panel.locator('[data-ui-choice]').evaluateAll((els) => els.map((e) => `${e.textContent.trim()}:${e.getAttribute('aria-checked')}`));
		expect(items.join(' ') === 'Current:false Beta:true', items.join(' '));
		expect(await panel.locator('[data-ui-note]').isVisible(), 'no note for Beta');
		await panel.locator('[data-ui-choice=current]').click();
		const t = await state();
		expect(t.ui === null && t.bg === DARK && t.stored === null, JSON.stringify(t));
		expect(!(await panel.locator('[data-ui-note]').isVisible()), 'the note stays for Current');
		await panel.locator('[data-ui-choice=beta]').click();
		expect((await state()).ui === 'beta', 'the keys menu did not choose Beta');
	});
	await check('look: the beta leaves the light theme as it is; Current clears the choice', async () => {
		await page.emulateMedia({ colorScheme: 'light' });
		await page.goto(FEED, { waitUntil: 'networkidle' });
		let s = await state();
		expect(s.ui === 'beta' && s.theme === 'light' && s.bg === LIGHT, JSON.stringify(s));
		await page.emulateMedia({ colorScheme: 'dark' });
		await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark', null, { timeout: 2000 }).catch(() => {});
		await page.locator('.mast .who').click();
		await page.locator('[data-ui-choice=current]').click();
		s = await state();
		expect(s.ui === null && s.bg === DARK && s.stored === null, JSON.stringify(s));
	});
	await context.close();
}

/** A menu's choices as `Label:aria-checked`, the theme's then the look's. */
const choices = (menu) =>
	menu.locator('[data-theme-choice], [data-ui-choice]').evaluateAll((els) => els.map((e) => `${e.textContent.trim()}:${e.getAttribute('aria-checked')}`).join(' '));
/** The root's theme and look. */
const root = (page) => page.evaluate(() => `${document.documentElement.dataset.theme} ${document.documentElement.dataset.ui ?? 'current'}`);
/** Lets a system preference change reach the page's listeners. */
const frames = (page) => page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));

// ---- the menu shows what the page applied, when the browser does not store the choice ----
{
	where = 'look unstored 1440';
	const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
	await context.addInitScript(() => {
		Storage.prototype.setItem = () => {
			throw new DOMException('full', 'QuotaExceededError');
		};
	});
	const page = await context.newPage();
	await check('look: on the keys page with storage failing, Beta and Light stay checked once chosen', async () => {
		await page.goto(`${FEED}keys`, { waitUntil: 'networkidle' });
		await page.locator('.mast .who').click();
		const menu = page.locator('.mast [role=menu]');
		await menu.locator('[data-ui-choice=beta]').click();
		let r = await root(page);
		let c = await choices(menu);
		expect(r === 'dark beta' && c === 'System:true Light:false Dark:false Current:false Beta:true', `${r} | ${c}`);
		expect(await menu.locator('[data-ui-note]').isVisible(), 'no note for Beta');
		await menu.locator('[data-theme-choice=light]').click();
		r = await root(page);
		c = await choices(menu);
		expect(r === 'light beta' && c === 'System:false Light:true Dark:false Current:false Beta:true', `${r} | ${c}`);
	});
	for (const [label, url, wait] of [
		['keys page', `${FEED}keys`, '.mast .who'],
		['app', FEED, '.mast li b']
	])
		await check(`look: in the ${label} with storage failing, a system change keeps the chosen Dark and Beta`, async () => {
			await page.emulateMedia({ colorScheme: 'dark' });
			await page.goto(url, { waitUntil: 'networkidle' });
			await page.waitForSelector(wait);
			await page.locator('.mast .who').click();
			const menu = page.locator('.mast [role=menu]');
			await menu.locator('[data-theme-choice=dark]').click();
			await menu.locator('[data-ui-choice=beta]').click();
			await page.emulateMedia({ colorScheme: 'light' });
			await frames(page);
			const r = await root(page);
			const c = await choices(menu);
			expect(r === 'dark beta' && c === 'System:false Light:false Dark:true Current:false Beta:true', `${r} | ${c}`);
		});
	await context.close();
}

// ---- another tab's choice reaches the page and its menu ----
{
	where = 'look tabs 1440';
	const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
	const other = await context.newPage();
	await other.goto(`${FEED}keys`, { waitUntil: 'networkidle' });
	for (const [label, url, wait] of [
		['keys page', `${FEED}keys`, '.mast .who'],
		['app', FEED, '.mast li b']
	])
		await check(`look: another tab's Light and Beta reach the ${label} and its menu`, async () => {
			await other.evaluate(() => localStorage.clear());
			const page = await context.newPage();
			await page.goto(url, { waitUntil: 'networkidle' });
			await page.waitForSelector(wait);
			await page.locator('.mast .who').click();
			const menu = page.locator('.mast [role=menu]');
			await other.evaluate(() => {
				localStorage.setItem('conde.theme', 'light');
				localStorage.setItem('conde.ui', 'beta');
			});
			await page.waitForFunction(() => document.documentElement.dataset.ui === 'beta', null, { timeout: 2000 }).catch(() => {});
			const r = await root(page);
			const c = await choices(menu);
			expect(r === 'light beta' && c === 'System:false Light:true Dark:false Current:false Beta:true', `${r} | ${c}`);
			expect(await menu.getByText('The beta restyles the dark theme.').isVisible(), 'no note for Beta');
			await page.close();
		});
	for (const [label, url, wait] of [
		['keys page', `${FEED}keys`, '.mast .who'],
		['app', FEED, '.mast li b']
	])
		await check(`look: with storage failing in the ${label}, another tab's write leaves its menu on what the page shows`, async () => {
			await other.evaluate(() => localStorage.clear());
			const page = await context.newPage();
			await page.addInitScript(() => {
				Storage.prototype.setItem = () => {
					throw new DOMException('full', 'QuotaExceededError');
				};
			});
			await page.goto(url, { waitUntil: 'networkidle' });
			await page.waitForSelector(wait);
			await page.locator('.mast .who').click();
			const menu = page.locator('.mast [role=menu]');
			await menu.locator('[data-ui-choice=beta]').click();
			await other.evaluate(() => localStorage.setItem('conde.other', '1'));
			await page.waitForFunction(() => document.documentElement.dataset.ui !== 'beta', null, { timeout: 1000 }).catch(() => {});
			const r = await root(page);
			const c = await choices(menu);
			expect(c.endsWith(r.endsWith('beta') ? 'Current:false Beta:true' : 'Current:true Beta:false'), `${r} | ${c}`);
			await page.close();
		});
	await context.close();
}

// ---- the Machine page: no machine, "Set up my agent", the request's progress; once the machine
// joined, the sign-ins: Claude's link and pasted code, GitHub's device code and its cancel (the mock
// keeps the machine, so the phone pass sees the joined machine and Claude's long link) ----
for (const size of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
	where = `machine ${size.width}`;
	const phone = size.width < 720;
	const context = await browser.newContext({ viewport: size, ...(phone ? { isMobile: true, hasTouch: true } : {}), locale: 'en-GB' });
	const page = await context.newPage();
	await check(phone ? 'machine: a tab of its own, Keys in More, the sign-in link fits' : 'machine: listed, setup offered, the request shown after the click, then the sign-ins', async () => {
		await page.goto(`${FEED}#/machine`, { waitUntil: 'networkidle' });
		const bar = phone ? '.tabs' : '.mast .sections';
		const labels = await page.locator(`${bar} a`).allTextContents();
		expect(labels.includes('Machine'), labels.join(','));
		expect(!phone || !labels.includes('Keys'), `Keys is in More on the phone: ${labels.join(',')}`);
		const current = await page.locator(`${bar} a[aria-current=page]`).textContent();
		expect(current === 'Machine', `current section ${current}`);
		const state = page.locator('.machine .state');
		await state.waitFor();
		const fits = async (what) => {
			const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
			expect(!overflow, `the page scrolls sideways (${what})`);
		};
		if (!phone) {
			expect((await state.textContent()) === 'Your agent has no machine yet.', await state.textContent());
			await page.getByRole('button', { name: 'Set up my agent' }).click();
			await page.locator('.machine .state[data-state=requested]').waitFor({ timeout: 2000 });
			const detail = await page.locator('.machine .detail').textContent();
			expect(/^Requested \d+ (s|min) ago · Azure: running\.$/.test(detail ?? ''), detail);
			expect((await page.getByRole('button', { name: 'Set up my agent' }).count()) === 0, 'the setup button stays after the request');
			expect((await page.locator('.machine .host').textContent()) === 'vm-agent-kdobmayer', 'the host');
			expect((await page.locator('.machine .flows button').count()) === 0, 'no sign-in before the machine joined');
			await fits('requested');
			await page.request.post(`${FEED}mock/machine-joined`);
			await page.reload({ waitUntil: 'networkidle' });
		}
		await page.locator('.machine .state[data-state=connected]').waitFor({ timeout: 2000 });
		const flows = await page.locator('.machine .flows button').allTextContents();
		expect(flows.join('|') === 'Sign in to Claude|Sign in to GitHub|Sign in to Codex', flows.join('|'));
		await page.getByRole('button', { name: 'Sign in to Claude' }).click();
		const link = page.locator('.machine .relay .link a');
		await link.waitFor({ timeout: 2000 });
		expect((await link.textContent()) === 'Open the Claude sign-in', await link.textContent());
		expect((await link.getAttribute('target')) === '_blank', 'the link opens beside the page');
		expect((await link.getAttribute('href'))?.startsWith('https://claude.com/cai/oauth/authorize'), await link.getAttribute('href'));
		await fits('the Claude link');
		const send = page.getByRole('button', { name: 'Send code' });
		expect(await send.isDisabled(), 'Send code waits for a code');
		if (phone) {
			await page.getByRole('button', { name: 'Cancel' }).click();
			await page.locator('.machine .flows').waitFor({ timeout: 2000 });
			expect((await page.locator('.machine .outcome').count()) === 0, 'a cancel says nothing');
			return;
		}
		await page.getByLabel('The code from Claude').fill('  mock-code#123 ');
		await send.click();
		await page.locator('.machine .outcome').waitFor({ timeout: 2000 });
		expect((await page.locator('.machine .outcome').textContent()) === 'Signed in to Claude.', await page.locator('.machine .outcome').textContent());
		expect((await page.locator('.machine .relay').count()) === 0, 'the sign-in part closes');
		await page.getByRole('button', { name: 'Sign in to GitHub' }).click();
		const usercode = page.locator('.machine .relay .usercode');
		await usercode.waitFor({ timeout: 2000 });
		expect((await usercode.textContent()) === 'WDJB-MJHT', await usercode.textContent());
		expect((await link.textContent()) === 'github.com/login/device', await link.textContent());
		expect((await page.getByLabel('The code from Claude').count()) === 0, 'GitHub takes no pasted code');
		await page.getByRole('button', { name: 'Cancel' }).click();
		await page.locator('.machine .flows').waitFor({ timeout: 2000 });
		expect((await page.locator('.machine .outcome').count()) === 0, 'a cancel says nothing');
		await fits('connected');
	});
	await context.close();
}

// ---- the phone's menu ("More") shows the beta's note too ----
{
	where = 'look 390';
	const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, colorScheme: 'dark' });
	const page = await context.newPage();
	await check('look: on the keys page at 390, More then Beta shows that menu’s note', async () => {
		await page.goto(`${FEED}keys`, { waitUntil: 'networkidle' });
		await page.locator('.tabs .who').click();
		const menu = page.locator('.tabs [role=menu]');
		await menu.locator('[data-ui-choice=beta]').click();
		expect((await root(page)) === 'dark beta', await root(page));
		expect(await menu.locator('[data-ui-note]').isVisible(), 'the More menu shows no note for Beta');
	});
	await context.close();
}

await browser.close();
mock.kill();
console.log(failed ? `${failed} check(s) failed` : 'all checks passed');
process.exit(failed ? 1 : 0);
