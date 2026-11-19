import { url } from '../lib/landing';

export function GET() {
  return new Response(`User-agent: *\nAllow: /\n\nSitemap: ${url('/sitemap.xml')}\n`, { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
}
