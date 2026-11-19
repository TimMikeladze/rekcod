import { url } from '../lib/landing';

export const pages = ['/', '/docs/'];

export function GET() {
  const urls = pages.map((p) => `<url><loc>${url(p)}</loc></url>`).join('');
  return new Response(`<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${urls}</urlset>`, {
    headers: { 'Content-Type': 'application/xml; charset=utf-8' },
  });
}
