// Design-only: drives the primitives harness (routes/_design/primitives) headless, by keyboard at
// 1440×900 (and the overlay layout at 1180×820) and by touch at 390×844, and exits non-zero when a
// check fails. Serves the built app through mock-server.mjs.
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
const URL = `http://127.0.0.1:${port}/#/_design/primitives`;

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

async function open(size) {
	where = `${size.width}`;
	const context = await browser.newContext({
		viewport: size,
		...(size.width < 720 ? { isMobile: true, hasTouch: true } : {}),
		reducedMotion: 'reduce'
	});
	const page = await context.newPage();
	page.on('pageerror', (e) => {
		failed++;
		console.log(`not ok  page error: ${e.message}`);
	});
	await page.goto(URL, { waitUntil: 'networkidle' });
	await page.waitForSelector('[data-test=open-sheet]');
	return { context, page };
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
const filters = (page) => page.locator('[data-test=filters]').textContent();
const listboxes = (page) => page.locator('[role=listbox]').count();
const dialogOpen = (page) => page.evaluate(() => document.querySelector('[data-test=main-sheet] dialog')?.open ?? false);
const pageState = (page) => page.locator('[data-test=page-state]').textContent();
const settle = (page) => page.waitForTimeout(400);
const box = (page, sel) => page.locator(sel).boundingBox();

// ---- desktop, keyboard ----
{
	const { context, page } = await open({ width: 1440, height: 900 });
	const dirButton = page.locator('[data-test=menu-dir] button');

	await check('filter menu: Tab reaches the menu button', async () => {
		await page.locator('[data-test=menu-agent] button').focus();
		await page.keyboard.press('Tab');
		const f = await focus(page);
		expect(f.test === 'menu-dir' && f.tag === 'button', `focus on ${JSON.stringify(f)}`);
	});
	await check('filter menu: Enter opens at the selected option, focus in the list', async () => {
		await page.keyboard.press('Enter');
		const f = await focus(page);
		expect((await listboxes(page)) === 1, 'no listbox');
		expect((await dirButton.getAttribute('aria-expanded')) === 'true', 'aria-expanded not true');
		expect(f.role === 'listbox' && f.active === 'all', `focus ${JSON.stringify(f)}`);
	});
	await check('filter menu: arrows, Home and End move; ArrowUp stops at the top', async () => {
		const seen = [];
		for (const key of ['ArrowDown', 'ArrowDown', 'ArrowUp', 'End', 'Home', 'ArrowUp']) {
			await page.keyboard.press(key);
			seen.push((await focus(page)).active);
		}
		expect(seen.join(',') === 'received,sent,received,between yours,all,all', seen.join(','));
	});
	await check('filter menu: Enter selects, closes and returns focus to the button', async () => {
		await page.keyboard.press('ArrowDown');
		await page.keyboard.press('Enter');
		const f = await focus(page);
		expect((await listboxes(page)) === 0, 'list still open');
		expect(f.test === 'menu-dir' && f.tag === 'button', `focus ${JSON.stringify(f)}`);
		expect((await filters(page)) === 'agent=all;dir=in', await filters(page));
		expect((await dirButton.textContent()).includes('received'), 'button does not show the value');
	});
	await check('filter menu: Space opens; Escape closes without a change, focus on the button', async () => {
		await page.keyboard.press(' ');
		expect((await focus(page)).active === 'received', 'opened away from the selected option');
		await page.keyboard.press('ArrowDown');
		await page.keyboard.press('Escape');
		const f = await focus(page);
		expect((await listboxes(page)) === 0, 'list still open');
		expect(f.test === 'menu-dir' && f.tag === 'button', `focus ${JSON.stringify(f)}`);
		expect((await filters(page)) === 'agent=all;dir=in', await filters(page));
	});
	await check('filter menu: ArrowDown opens; Tab closes it and moves on to the next control', async () => {
		await page.keyboard.press('ArrowDown');
		expect((await listboxes(page)) === 1, 'ArrowDown did not open');
		await page.keyboard.press('Tab');
		expect((await listboxes(page)) === 0, 'Tab left it open');
		const f = await focus(page);
		expect(f.test === 'open-sheet', `focus ${JSON.stringify(f)}`);
	});
	await check('filter menu: a press outside closes it', async () => {
		await dirButton.focus();
		await page.keyboard.press('Enter');
		await page.mouse.click(5, 880);
		expect((await listboxes(page)) === 0, 'still open');
	});

	const hash = await page.evaluate(() => location.hash);
	await check('sheet: Enter opens it as a modal dialog with focus inside', async () => {
		await page.locator('[data-test=open-sheet]').focus();
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => document.querySelector('[data-test=main-sheet] dialog')?.open);
		const f = await focus(page);
		expect(f.inDialog, `focus ${JSON.stringify(f)}`);
	});
	await check('sheet: from 720 px it is a 500 px panel on the right, full height', async () => {
		const b = await box(page, 'dialog');
		expect(Math.round(b.x + b.width) === 1440 && Math.round(b.width) === 500 && Math.round(b.height) === 900, JSON.stringify(b));
	});
	await check('sheet: Tab and Shift+Tab stay inside', async () => {
		for (const key of [...Array(8).fill('Tab'), ...Array(8).fill('Shift+Tab')]) {
			await page.keyboard.press(key);
			const f = await focus(page);
			expect(f.inDialog, `after ${key}: ${JSON.stringify(f)}`);
		}
	});
	await check('sheet: the page behind is inert', async () => {
		const b = await box(page, '[data-test=behind]');
		await page.mouse.click(b.x + b.width / 2, b.y + b.height / 2);
		expect((await page.locator('[data-test=behind]').textContent()).includes('(0)'), 'the button behind took the click');
		expect(await dialogOpen(page), 'a click behind closed it');
	});
	await check('sheet: nothing animates (reduced motion)', async () => {
		const n = await page.evaluate(() => document.getAnimations().length);
		expect(n === 0, `${n} animations`);
	});
	await check('sheet: Escape closes it; focus returns to the opener; the URL is unchanged', async () => {
		await page.keyboard.press('Escape');
		await page.waitForFunction(() => !document.querySelector('[data-test=main-sheet] dialog')?.open);
		const f = await focus(page);
		expect(f.test === 'open-sheet', `focus ${JSON.stringify(f)}`);
		expect((await page.evaluate(() => location.hash)) === hash, await page.evaluate(() => location.hash));
	});
	await check('sheet: browser back closes it; focus returns to the opener', async () => {
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => document.querySelector('[data-test=main-sheet] dialog')?.open);
		await page.goBack();
		await page.waitForFunction(() => !document.querySelector('[data-test=main-sheet] dialog')?.open);
		const f = await focus(page);
		expect(f.test === 'open-sheet', `focus ${JSON.stringify(f)}`);
		expect((await page.evaluate(() => location.hash)) === hash, 'left the page');
	});
	await check('sheet: Close closes it; reopening still works', async () => {
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => document.querySelector('[data-test=main-sheet] dialog')?.open);
		await page.locator('[data-test=main-sheet] dialog header button').click();
		await page.waitForFunction(() => !document.querySelector('[data-test=main-sheet] dialog')?.open);
		expect((await focus(page)).test === 'open-sheet', 'focus not on the opener');
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => document.querySelector('[data-test=main-sheet] dialog')?.open);
		await page.keyboard.press('Escape');
		await page.waitForFunction(() => !document.querySelector('[data-test=main-sheet] dialog')?.open);
	});
	await check('sheet: the parent closing it at once leaves no history entry behind', async () => {
		await page.locator('[data-test=race-parent]').click();
		await settle(page);
		expect(!(await dialogOpen(page)), 'still open');
		expect((await pageState(page)) === '{}', `history state ${await pageState(page)}`);
	});
	await check('sheet: Close before the history entry exists keeps the page and closes once it does', async () => {
		await page.locator('[data-test=race-dismiss]').click();
		await settle(page);
		expect((await page.evaluate(() => location.hash)) === hash, `left the page: ${page.url()}`);
		if (await dialogOpen(page)) {
			await page.keyboard.press('Escape');
			await settle(page);
		}
		expect(!(await dialogOpen(page)), 'still open');
		expect((await pageState(page)) === '{}', `history state ${await pageState(page)}`);
	});
	await check('sheet: closed by the parent, reopened, then back closes it', async () => {
		await page.locator('[data-test=parent-later]').click();
		await settle(page);
		expect(!(await dialogOpen(page)), 'the parent did not close it');
		expect((await pageState(page)) === '{}', `history state after the parent closed it: ${await pageState(page)}`);
		await page.locator('[data-test=open-sheet]').focus();
		await page.keyboard.press('Enter');
		await page.waitForFunction(() => document.querySelector('[data-test=main-sheet] dialog')?.open);
		await settle(page);
		await page.goBack();
		await settle(page);
		expect(!(await dialogOpen(page)), 'back left it open');
		expect((await page.evaluate(() => location.hash)) === hash, 'left the page');
	});
	await check('sheet: mounted with open set, it stays open and back closes it', async () => {
		await page.locator('[data-test=mount-open]').click();
		await settle(page);
		const mounted = () => page.evaluate(() => document.querySelector('[data-test=mounted-sheet] dialog')?.open ?? false);
		expect(await mounted(), 'closed itself');
		await page.goBack();
		await settle(page);
		expect(!(await mounted()), 'back left it open');
		expect((await page.evaluate(() => location.hash)) === hash, 'left the page');
		expect((await pageState(page)) === '{}', `history state ${await pageState(page)}`);
	});

	await check('disclosure: Enter and Space toggle aria-expanded and the region', async () => {
		const toggle = page.locator('[data-test=disc-konstantin] button');
		const region = page.locator('[data-test=disc-konstantin] [id$=-region]');
		await toggle.focus();
		expect((await toggle.getAttribute('aria-expanded')) === 'false' && !(await region.isVisible()), 'not closed at first');
		await page.keyboard.press('Enter');
		expect((await toggle.getAttribute('aria-expanded')) === 'true' && (await region.isVisible()), 'Enter did not open it');
		await page.keyboard.press(' ');
		expect((await toggle.getAttribute('aria-expanded')) === 'false' && !(await region.isVisible()), 'Space did not close it');
	});
	await check('error line: Retry by keyboard calls retry', async () => {
		await page.getByRole('button', { name: 'Retry' }).focus();
		await page.keyboard.press('Enter');
		expect((await page.locator('[data-test=retries]').textContent()) === '1', 'retry not called');
		expect(/as of \d\d:\d\d/.test(await page.getByRole('alert').textContent()), 'no "as of hh:mm"');
	});
	await check('announcer: arrivals are said in a polite live region', async () => {
		await page.locator('[data-test=arrive]').click();
		expect((await page.locator('[aria-live=polite]').textContent()) === '2 new messages', 'not announced');
	});
	await context.close();
}

// ---- 1180: the overlay ----
{
	const { context, page } = await open({ width: 1180, height: 820 });
	await check('sheet at 1180: a 500 px overlay on the right, modal', async () => {
		await page.locator('[data-test=open-sheet]').click();
		await page.waitForFunction(() => document.querySelector('[data-test=main-sheet] dialog')?.open);
		const b = await box(page, 'dialog');
		expect(Math.round(b.x + b.width) === 1180 && Math.round(b.width) === 500 && Math.round(b.height) === 820, JSON.stringify(b));
		expect(!(await page.locator('dialog .handle').isVisible()), 'the handle shows on the overlay');
		await page.keyboard.press('Escape');
		await page.waitForFunction(() => !document.querySelector('[data-test=main-sheet] dialog')?.open);
	});
	await context.close();
}

// ---- phone, touch ----
{
	const { context, page } = await open({ width: 390, height: 844 });
	await check('filter menu by touch: tap opens, tapping an option selects and closes', async () => {
		await page.locator('[data-test=menu-dir] button').tap();
		expect((await listboxes(page)) === 1, 'tap did not open');
		await page.getByRole('option', { name: 'between yours' }).tap();
		expect((await listboxes(page)) === 0, 'still open');
		expect((await filters(page)) === 'agent=all;dir=own', await filters(page));
	});
	await check('sheet by touch: a bottom sheet over the page', async () => {
		await page.locator('[data-test=open-sheet]').tap();
		await page.waitForFunction(() => document.querySelector('[data-test=main-sheet] dialog')?.open);
		const b = await box(page, 'dialog');
		expect(Math.round(b.width) === 390 && Math.round(b.y + b.height) === 844 && b.height < 844 * 0.8, JSON.stringify(b));
	});
	await check('sheet by touch: the page behind takes no tap', async () => {
		const sheet = await box(page, '[data-test=main-sheet] dialog');
		const b = await box(page, '[data-test=behind]');
		const [x, y] = [b.x + b.width / 2, b.y + b.height / 2];
		expect(y < sheet.y, `the button behind is under the sheet (${y} >= ${sheet.y})`);
		await page.touchscreen.tap(x, y);
		expect((await page.locator('[data-test=behind]').textContent()).includes('(0)'), 'behind took it');
		expect(await dialogOpen(page), 'a tap behind closed it');
	});
	await check('sheet by touch: the handle toggles full height', async () => {
		const handle = page.locator('dialog .handle');
		await handle.tap();
		let b = await box(page, 'dialog');
		expect(Math.round(b.height) === 844 && (await handle.getAttribute('aria-expanded')) === 'true', JSON.stringify(b));
		await handle.tap();
		b = await box(page, 'dialog');
		expect(b.height < 844 * 0.8 && (await handle.getAttribute('aria-expanded')) === 'false', JSON.stringify(b));
	});
	await check('sheet by touch: back closes it', async () => {
		await page.goBack();
		await page.waitForFunction(() => !document.querySelector('[data-test=main-sheet] dialog')?.open);
		expect((await focus(page)).test === 'open-sheet', 'focus not on the opener');
	});
	await check('disclosure by touch: tap toggles', async () => {
		const toggle = page.locator('[data-test=disc-balazs] button');
		await toggle.tap();
		expect((await toggle.getAttribute('aria-expanded')) === 'true', 'tap did not open');
		await toggle.tap();
		expect((await toggle.getAttribute('aria-expanded')) === 'false', 'tap did not close');
	});
	await context.close();
}

await browser.close();
mock.kill();
console.log(failed ? `${failed} check(s) failed` : 'all checks passed');
process.exit(failed ? 1 : 0);
