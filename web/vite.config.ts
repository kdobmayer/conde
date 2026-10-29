import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// The theme script inline in app.html, allowed by its hash (test/theme.test.mjs checks it).
const themeHash = 'sha256-Hof4aQPFx2uDmN649sesR+jaKFXfTdz8GmXIkxB5pDI=';

// The UI is embedded in the conde server binary: the shell at `/`, everything else under `/app/`.
export default defineConfig({
	plugins: [
		sveltekit({
			adapter: adapter({
				pages: '../crates/conde-remote/web-dist',
				strict: true
			}),
			appDir: 'app',
			router: { type: 'hash' },
			csp: {
				mode: 'hash',
				directives: {
					'default-src': ['self'],
					'script-src': ['self', themeHash],
					'style-src': ['self', 'unsafe-inline'],
					'img-src': ['self', 'data:'],
					'connect-src': ['self'],
					'object-src': ['none'],
					'base-uri': ['none']
				}
			},
			output: { bundleStrategy: 'single' }
		})
	]
});
