import { getDocuments, getNavigation } from '../lib/content';

export async function GET() {
  const documents = await getDocuments();
  const order = getNavigation(documents).flatMap((group) => group.items.map((item) => item.url));
  const rank = (url: string) => { const index = order.indexOf(url); return index < 0 ? order.length : index; };
  documents.sort((a, b) => rank(a.url) - rank(b.url));
  return new Response(JSON.stringify(documents.map(({ title, url, text }) => ({ title, url, text }))), {
    headers: { 'Content-Type': 'application/json; charset=utf-8' },
  });
}
