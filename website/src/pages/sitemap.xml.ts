import type { APIContext } from 'astro';
import { getDocuments } from '../lib/content';

export async function GET({ site }: APIContext) {
  const documents = await getDocuments();
  const urls = documents.map((doc) => `<url><loc>${new URL(doc.url, site)}</loc></url>`).join('');
  return new Response(`<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${urls}</urlset>`, {
    headers: { 'Content-Type': 'application/xml; charset=utf-8' },
  });
}
