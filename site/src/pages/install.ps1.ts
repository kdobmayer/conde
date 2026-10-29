import type { APIRoute } from 'astro';
// Served at /install.ps1: the Windows installer from dist/install.ps1, inlined at build time; `irm | iex` reads it as text.
import script from '../../../dist/install.ps1?raw';

export const GET: APIRoute = () =>
	new Response(script, { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
