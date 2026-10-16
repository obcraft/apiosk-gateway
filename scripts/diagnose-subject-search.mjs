#!/usr/bin/env node
// Read public search responses only. No app credentials, model calls or purchases.
import { mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const subject = process.argv.slice(2).join(' ').trim();
if (!subject) throw new Error('Usage: node gateway/scripts/diagnose-subject-search.mjs "Subject name"');
const directory = new URL('../.local/subject-search-debug/', import.meta.url);
await mkdir(directory, { recursive: true });
const query = `${subject} official website`;
const google = new URL('https://www.google.com/search');
google.searchParams.set('q', query);
google.searchParams.set('num', '5');
const bing = new URL('https://www.bing.com/search');
bing.searchParams.set('q', query);
bing.searchParams.set('format', 'rss');
const duckduckgo = new URL('https://html.duckduckgo.com/html/');
duckduckgo.searchParams.set('q', subject);
const wikipedia = new URL('https://en.wikipedia.org/w/api.php');
for (const [key, value] of Object.entries({ action: 'query', generator: 'search', gsrsearch: subject,
  gsrlimit: '3', prop: 'extracts|info', exintro: '1', exlimit: '3', explaintext: '1', inprop: 'url', format: 'json' })) wikipedia.searchParams.set(key, value);
const report = await Promise.all([
  ['google', google, 'text/html,application/xhtml+xml'],
  ['bing', bing, 'application/rss+xml,application/xml,text/xml'],
  ['duckduckgo', duckduckgo, 'text/html,application/xhtml+xml'],
  ['wikipedia', wikipedia, 'application/json'],
].map(async ([provider, url, accept]) => {
  try {
    const response = await fetch(url, {
      headers: { accept, 'accept-encoding': 'identity', 'user-agent': 'Apiosk-LinkPreview/1.0 (+https://apiosk.com)' },
      signal: AbortSignal.timeout(6000),
    });
    const reader = response.body.getReader();
    const chunks = [];
    let bytes = 0;
    while (bytes < 512 * 1024) {
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = value.subarray(0, 512 * 1024 - bytes);
      chunks.push(chunk);
      bytes += chunk.length;
    }
    await reader.cancel();
    const body = Buffer.concat(chunks);
    await writeFile(new URL(`${provider}.txt`, directory), body);
    return { provider, requested: String(url), final: response.url, status: response.status, type: response.headers.get('content-type'), bytes };
  } catch (error) {
    return { provider, error: String(error.cause ?? error) };
  }
}));
await writeFile(new URL('report.json', directory), JSON.stringify(report, null, 2));
console.log(JSON.stringify(report, null, 2));
console.log(`Saved public responses to ${fileURLToPath(directory)}`);
