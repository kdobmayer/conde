// Drives the app's Feed and Fleet pages headless on the mock API: the filter menu, the thread
// beside the Feed and in the sheet, the disclosure, the error line, the live announcer, the
// agent list's resize handle and markdown in message bodies, by
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
	await check('agent list: the default is 290 px; dragging the handle stops at 200 and 480 px', async () => {
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
		await page.mouse.move(c.x + 600, y, { steps: 6 });
		await page.mouse.up();
		expect((await listWidth()) === 480 && (await stored()) === '480', `wide: ${await listWidth()}, stored ${await stored()}`);
	});
	await check('agent list: the width survives a reload', async () => {
		await page.reload({ waitUntil: 'networkidle' });
		await page.waitForSelector('.msg');
		expect((await listWidth()) === 480, `${await listWidth()}`);
	});
	await check('agent list: the handle is a focusable separator; arrows resize, Home goes to 200, aria-valuenow follows', async () => {
		await handle.focus();
		const f = await focus(page);
		expect(f.role === 'separator', JSON.stringify(f));
		await page.keyboard.press('ArrowLeft');
		expect((await listWidth()) === 464 && (await handle.getAttribute('aria-valuenow')) === '464', `${await listWidth()} / ${await handle.getAttribute('aria-valuenow')}`);
		await page.keyboard.press('Shift+ArrowLeft');
		expect((await listWidth()) === 416, `${await listWidth()}`);
		await page.keyboard.press('ArrowRight');
		expect((await listWidth()) === 432 && (await stored()) === '432', `${await listWidth()}, stored ${await stored()}`);
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
		const items = await page.locator('[role=menu] [role=menuitemradio]').evaluateAll((els) => els.map((e) => `${e.textContent.trim()}:${e.getAttribute('aria-checked')}`));
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

await browser.close();
mock.kill();
console.log(failed ? `${failed} check(s) failed` : 'all checks passed');
process.exit(failed ? 1 : 0);
