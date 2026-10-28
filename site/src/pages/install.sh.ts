import type { APIRoute } from 'astro';
// Served at /install.sh: the installer from dist/install.sh at the repository root, inlined at build time.
import script from '../../../dist/install.sh?raw';

export const GET: APIRoute = () =>
	new Response(script, { headers: { 'Content-Type': 'text/x-shellscript; charset=utf-8' } });
