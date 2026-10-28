import type { APIRoute, GetStaticPaths } from 'astro';
import { getCollection } from 'astro:content';

// Every docs page as plain Markdown at its own path plus .md (/docs/install.md, /privacy.md).
export const getStaticPaths: GetStaticPaths = async () =>
	(await getCollection('docs')).map((entry) => ({ params: { slug: entry.id }, props: { entry } }));

export const GET: APIRoute = ({ props, site }) => {
	const { entry } = props;
	const base = site?.href.replace(/\/$/, '') ?? '';
	// Site-relative links point at the HTML; an agent reading this copy wants the .md twins.
	const body = (entry.body ?? '').replace(/\]\(\/(docs\/[a-z-]+|privacy)\)/g, `](${base}/$1.md)`).replace(/\]\(\//g, `](${base}/`);
	const text = `# ${entry.data.title}\n\n${entry.data.description ? entry.data.description + '\n\n' : ''}${body}`;
	return new Response(text, { headers: { 'Content-Type': 'text/markdown; charset=utf-8' } });
};
