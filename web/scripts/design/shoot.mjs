// Design-only: headless screenshots of pages on a local server, one at a time.
// Usage: node scripts/design/shoot.mjs <shots.json>
// shots.json: [{ "url": "...", "out": "path.png", "size": "desktop" | "tablet" | "phone", "scheme": "light" | "dark",
//                "width"?: px, "height"?: px (override the size's viewport),
//                "wait"?: "css selector", "click"?: "css selector",
//                "storage"?: { key: value } (localStorage before the page loads),
//                "fill"?: ["css selector", "text"] (typed after load, before "wait"),
//                "scroll"?: "css selector" (scrolled to the middle of its scroller after "wait") }]
// Needs playwright-core 1.64.0 (npm i --no-save) and a Chromium headless shell under
// ~/.cache/ms-playwright (CHROME overrides the path). The clock is fixed to the fixture's NOW
// in Europe/Budapest so relative times and day labels are stable.

import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { chromium } from 'playwright-core';

const shots = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const executablePath =
	process.env.CHROME ??
	`${homedir()}/.cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell`;
const NOW = new Date('2026-10-14T12:20:00Z');
const sizes = {
	desktop: { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 },
	tablet: { viewport: { width: 1180, height: 820 }, deviceScaleFactor: 2 },
	phone: { viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true }
};

const browser = await chromium.launch({ executablePath, args: ['--disable-gpu', '--font-render-hinting=none'] });
for (const s of shots) {
	const size = sizes[s.size];
	const context = await browser.newContext({
		...size,
		viewport: { width: s.width ?? size.viewport.width, height: s.height ?? size.viewport.height },
		colorScheme: s.scheme,
		locale: 'en-GB',
		timezoneId: 'Europe/Budapest',
		reducedMotion: 'reduce'
	});
	const page = await context.newPage();
	await page.clock.setFixedTime(NOW);
	if (s.storage) await page.addInitScript((entries) => Object.entries(entries).forEach(([k, v]) => localStorage.setItem(k, v)), s.storage);
	page.on('pageerror', (e) => console.error(`${s.out}: page error: ${e.message}`));
	await page.goto(s.url, { waitUntil: 'networkidle' });
	if (s.click) await page.click(s.click);
	if (s.fill) await page.fill(s.fill[0], s.fill[1]);
	if (s.wait) await page.waitForSelector(s.wait);
	if (s.scroll) await page.locator(s.scroll).evaluate((el) => el.scrollIntoView({ block: 'center' }));
	await page.waitForTimeout(400);
	await page.screenshot({ path: s.out });
	console.log(`wrote ${s.out}`);
	await context.close();
}
await browser.close();
