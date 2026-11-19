import { load } from '../lib/landing';
import { pageMarkdown } from '../lib/markdown';

export function GET() {
  return new Response(pageMarkdown(load().landing), { headers: { 'Content-Type': 'text/markdown; charset=utf-8' } });
}
