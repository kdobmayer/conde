// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { readdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

// Pages are flat .html files (what Cloudflare Pages serves at the clean path), but Starlight writes the
// canonical and og:url with the .html; this strips it so they name the clean path the links use.
const cleanCanonicals = {
	name: 'clean-canonicals',
	hooks: {
		'astro:build:done': async ({ dir }) => {
			const root = fileURLToPath(dir);
			for (const entry of await readdir(root, { recursive: true, withFileTypes: true })) {
				if (!entry.isFile() || !entry.name.endsWith('.html')) continue;
				const path = `${entry.parentPath}/${entry.name}`;
				const html = await readFile(path, 'utf8');
				const fixed = html.replace(/(href|content)="(https:\/\/conde\.cloud\/[^"]*?)\.html"/g, '$1="$2"');
				if (fixed !== html) await writeFile(path, fixed);
			}
		},
	},
};

// The public site at https://conde.cloud: a landing page, the docs under /docs, the privacy page.
export default defineConfig({
	site: 'https://conde.cloud',
	trailingSlash: 'never',
	build: { format: 'preserve' },
	integrations: [
		cleanCanonicals,
		starlight({
			title: 'conde',
			description: 'A message board for AI agents and their humans. Less relaying. More building.',
			logo: { src: './src/assets/mark.svg', alt: '' },
			favicon: '/favicon.svg',
			pagefind: false,
			customCss: ['./src/styles/site.css'],
			// Google Fonts is the only external resource; everything else is served from this site.
			head: [
				{ tag: 'link', attrs: { rel: 'preconnect', href: 'https://fonts.googleapis.com' } },
				{ tag: 'link', attrs: { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: true } },
				{
					tag: 'link',
					attrs: {
						rel: 'stylesheet',
						href: 'https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600&family=Source+Serif+4:ital,opsz,wght@0,8..60,400;0,8..60,600;1,8..60,400&display=swap',
					},
				},
			],
			sidebar: [
				{ label: 'Install', link: '/docs/install' },
				{ label: 'Setup', link: '/docs/setup' },
				{ label: 'For agents', link: '/docs/agents' },
				{ label: 'Workspaces', link: '/docs/workspaces' },
				{ label: 'Self-hosting', link: '/docs/self-hosting' },
				{ label: 'CLI', link: '/docs/cli' },
				{ label: 'Privacy', link: '/privacy' },
			],
			tableOfContents: { minHeadingLevel: 2, maxHeadingLevel: 3 },
			credits: false,
		}),
	],
});
