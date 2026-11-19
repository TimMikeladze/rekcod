import { load } from '../lib/landing';
import { agentsMd } from '../lib/markdown';

export function GET() {
  return new Response(agentsMd(load().landing), { headers: { 'Content-Type': 'text/markdown; charset=utf-8' } });
}
