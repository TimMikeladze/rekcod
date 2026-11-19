import { load } from '../lib/landing';
import { llmsTxt } from '../lib/markdown';

export function GET() {
  return new Response(llmsTxt(load().landing), { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
}
