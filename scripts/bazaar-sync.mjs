#!/usr/bin/env node
// Coinbase Bazaar (x402 discovery) catalog import.
//
// DRY RUN BY DEFAULT: reads the public discovery API, applies the admission
// filter, and writes report.json, catalog.json, excluded.json and SQL chunks to
// an output directory. It never writes to a database unless --apply is given.
//
//   node scripts/bazaar-sync.mjs [--tier=500] [--out=output/bazaar/<ts>]
//        [--existing=output/catalog-coverage-now/catalog.json]
//        [--host-policy=policy.json] [--input=recorded-page.json ...]
//        [--min-score=8] [--max-per-host=40] [--apply]
//
// Each resource becomes one endpoint of a per-host service (`cbz-<host>`),
// owned by the `coinbase-bazaar` provider, so /v2/sources shows ONE grouped
// source "Coinbase Bazaar". Calls are resold through the existing resell path:
// the buyer's balance pays the provider price plus the Apiosk fee
// (catalog-core fees.rs), the treasury pays the provider's own 402.
//
// --apply (operator only, after the migration and a kill-switch flip): one sync
// run row, a diff on (host,method,path,content_hash), the batch RPC with at most
// 200 rows per call, one call at a time, 2 s apart, aborting when a call takes
// longer than 5 s, the gateway /health degrades or the kill switch turns off.
// Resources missed by two complete runs are retired (active=false), never deleted.
import crypto from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';
import { buildOperations } from './source-operation-contracts.mjs';

export const BAZAAR_URL = 'https://api.cdp.coinbase.com/platform/v2/x402/discovery/resources';
export const BASE_NETWORK = 'eip155:8453';
// USDC on Base, the only asset the treasury pays with (resell.rs, Sirenic guard).
export const BASE_USDC = '0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913';
// wallets.network_tokens row for USDC on Base (legacy-executor migration 103).
export const BASE_USDC_TOKEN_ID = '26adabc5-92ec-44f3-9540-3bd636eeb1f4';
export const MAX_PRICE_ATOMIC = 1_000_000n; // $1.00 in USDC atomic units
export const STALE_DAYS = 30;
export const MIN_LISTING_SCORE = 8; // legacy-executor bazaar.rs MIN_LISTING_SCORE
export const MAX_PER_HOST = 40; // legacy-executor bazaar.rs MAX_PER_HOST
export const BATCH_MAX_ROWS = 200;
export const BATCH_MAX_BYTES = 280_000;
export const SERVICE_PREFIX = 'cbz-';
const PAGE_LIMIT = 1000;
const PAGE_GAP_MS = 250; // >= 120 ms between pages
const MAX_PAGES = 60;
const MAX_EXAMPLE_BYTES = 16_384;
const MAX_SCHEMA_BYTES = 64 * 1024;

// ---------------------------------------------------------------------------
// Shared helpers (ids identical to source-operation-contracts.mjs)
// ---------------------------------------------------------------------------
const digest = x => crypto.createHash('sha256').update(JSON.stringify(x)).digest('hex');
export const uuid = x => { const h = digest(x); return `${h.slice(0, 8)}-${h.slice(8, 12)}-4${h.slice(13, 16)}-8${h.slice(17, 20)}-${h.slice(20, 32)}`; };
const isObject = v => v != null && typeof v === 'object' && !Array.isArray(v);
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

/** Atomic USDC as an exact decimal USD string: 3000 -> "0.003". */
export function atomicToUsd(atomic) {
  const n = BigInt(atomic);
  const whole = n / 1_000_000n, frac = (n % 1_000_000n).toString().padStart(6, '0').replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : `${whole}`;
}

// ---------------------------------------------------------------------------
// Hosts (legacy-executor bazaar.rs lists, matched as domain suffixes)
// ---------------------------------------------------------------------------
const BAD_HOST_SUFFIXES = ['localhost', 'local', 'test', 'invalid', 'internal', 'onion', 'example.com', 'example.org', 'example.net',
  'ngrok.io', 'ngrok-free.app', 'ngrok-free.dev', 'ngrok.app', 'ngrok.dev', 'trycloudflare.com', 'loca.lt', 'serveo.net', 'nip.io', 'sslip.io', 'xip.io'];
const PAAS_HOST_SUFFIXES = ['onrender.com', 'zeabur.app', 'run.app', 'herokuapp.com', 'railway.app', 'up.railway.app', 'fly.dev', 'replit.dev',
  'repl.co', 'glitch.me', 'vercel.app', 'netlify.app', 'pages.dev', 'workers.dev'];
export const isUnder = (host, suffix) => host === suffix || host.endsWith(`.${suffix}`);
export function unsafeHost(host) {
  if (!host || host.startsWith('[') || /^\d+(\.\d+){3}$/.test(host) || host === 'metadata.google.internal') return true;
  if (BAD_HOST_SUFFIXES.some(s => isUnder(host, s))) return true;
  const [left, tld] = [host.slice(0, host.lastIndexOf('.')), host.slice(host.lastIndexOf('.') + 1)];
  return !host.includes('.') || !left || tld.length < 2 || !/^[a-z]+$/.test(tld) || !/^[a-z0-9.-]+$/.test(host);
}
export const paasHost = host => PAAS_HOST_SUFFIXES.some(s => isUnder(host, s));
/** `cbz-<host>`: a valid /v2 service slug (<= 100 chars of [a-z0-9-]). */
export function serviceSlug(host) {
  const base = host.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
  const slug = `${SERVICE_PREFIX}${base}`;
  return slug.length <= 96 ? slug : `${slug.slice(0, 87)}-${digest(host).slice(0, 8)}`;
}

// ---------------------------------------------------------------------------
// Listing quality (ports of legacy-executor bazaar.rs; the same facts, not new judgement)
// ---------------------------------------------------------------------------
const TOY_MARKERS = ['dice', 'coinflip', 'coin-flip', 'hello world', 'helloworld', 'test endpoint', 'lorem ipsum', 'random number', 'guess the', '8ball', '8-ball', 'fortune cookie'];
export function looksLikeToy(text) {
  const lower = String(text ?? '').toLowerCase();
  const words = lower.split(/[^a-z0-9]+/);
  return TOY_MARKERS.some(m => (m.includes(' ') || m.includes('-')) ? lower.includes(m) : words.includes(m));
}
const JUNK_WORDS = ['test', 'testing', 'demo', 'example', 'sample', 'foo', 'bar', 'baz', 'ping', 'pong', 'asdf', 'todo', 'tbd', 'n/a', 'none', 'untitled'];
const JUNK_PHRASES = ['lorem ipsum', 'your description here', 'description here', 'add a description', 'no description', 'coming soon', 'placeholder'];
export function junkDescription(description) {
  const lower = String(description ?? '').trim().toLowerCase();
  if ([...lower].filter(c => /\p{L}/u.test(c)).length < 3) return true;
  if (JUNK_PHRASES.some(p => lower.includes(p))) return true;
  const words = lower.split(/[^a-z0-9/]+/).filter(Boolean);
  return words.length > 0 && words.every(w => JUNK_WORDS.includes(w));
}
const daysSince = (iso, now) => { const t = Date.parse(iso ?? ''); return Number.isFinite(t) ? Math.max(0, Math.floor((now - t) / 86_400_000)) : null; };
function outputHasShape(v) {
  if (!isObject(v)) return false;
  if (isObject(v.properties) && Object.keys(v.properties).length) return true;
  if ((isObject(v.example) && Object.keys(v.example).length) || (Array.isArray(v.example) && v.example.length)) return true;
  return Object.keys(v).some(k => k !== 'type');
}
function declaredInputs(item, info, schema) {
  const out = { named: false, typed: false, required: false, method: false };
  const props = schema?.properties;
  if (isObject(props) && Object.keys(props).length) {
    out.named = true;
    out.typed = Object.values(props).every(v => typeof v?.type === 'string' && v.type !== '');
    if (Array.isArray(schema.required) && schema.required.length) out.required = true;
  }
  for (const call of [item.inputSchema, info?.input]) {
    if (!isObject(call)) continue;
    if (typeof call.method === 'string') out.method = true;
    for (const slot of ['pathParams', 'queryParams', 'body', 'bodyFields', 'headers']) {
      const fields = call[slot];
      if (!isObject(fields) || !Object.keys(fields).length) continue;
      out.named = true;
      if (slot === 'pathParams') out.required = true;
      const defs = Object.values(fields).filter(isObject);
      if (defs.length === Object.keys(fields).length && defs.every(d => d.type !== undefined || d.enum !== undefined)) {
        out.typed = true;
        if (defs.some(d => d.required === true)) out.required = true;
      }
    }
  }
  if (typeof item.method === 'string') out.method = true;
  return out;
}
/** legacy bazaar.rs score_listing: ten documented facts, usage can only cap. */
export function scoreListing(item, accept, now = Date.now()) {
  const bazaar = item.extensions?.bazaar ?? accept?.extensions?.bazaar;
  const info = bazaar?.info, schema = bazaar?.schema;
  let points = 0;
  const description = String(item.description ?? accept?.description ?? '').trim();
  const chars = [...description].length, words = description.split(/\s+/).filter(Boolean).length;
  if (chars >= 80 && words >= 12) points++;
  if (chars >= 160 && words >= 24) points++;
  if (chars >= 320 && words >= 48) points++;
  const inputs = declaredInputs(item, info, schema);
  points += Number(inputs.named) + Number(inputs.typed) + Number(inputs.required);
  const outputs = [info?.output, schema?.output, item.outputSchema].filter(v => v != null);
  if (outputs.length) points++;
  if (outputs.some(outputHasShape)) points++;
  const priced = (accept?.amount ?? accept?.maxAmountRequired) != null;
  const assetNamed = typeof accept?.extra?.name === 'string' && accept.extra.name.trim() !== '';
  if (priced && assetNamed) points++;
  const railNamed = typeof accept?.network === 'string' && accept.network.trim() !== '' && accept.network.toLowerCase() !== 'unknown';
  if (railNamed && inputs.method) points++;
  const documented = Math.min(points, 10);
  const q = item.quality;
  let cap = null;
  if (isObject(q) && Number.isInteger(q.l30DaysUniquePayers)) {
    const days = daysSince(q.lastCalledAt, now);
    const stale = days == null || days > STALE_DAYS;
    if (q.l30DaysUniquePayers === 0 || stale) cap = [4, 'nobody paid for it in the last 30 days'];
    else if (q.l30DaysUniquePayers < 2) cap = [7, 'only one payer in the last 30 days'];
  }
  return cap && documented > cap[0] ? { score: cap[0], documented, capped: cap[1] } : { score: documented, documented, capped: null };
}

// ---------------------------------------------------------------------------
// Input schemas (port of gateway/src/ask_v2/external.rs checked_schema/fields
// plus catalog-core facts/schema.rs check_schema)
// ---------------------------------------------------------------------------
const SUPPORTED = new Set(['type', 'required', 'properties', 'items', 'enum', 'pattern', 'minimum', 'maximum', 'minLength', 'maxLength', 'additionalProperties']);
const ANNOTATIONS = new Set(['title', 'description', 'examples', 'default', '$comment']);
const STRIPPED = ['$schema', '$id', 'format', 'example', 'deprecated', 'readOnly', 'writeOnly'];
const TYPES = new Set(['object', 'array', 'string', 'number', 'integer', 'boolean', 'null']);
class SchemaError extends Error {}
const schemaFail = detail => { throw new SchemaError(detail); };
export const validName = name => typeof name === 'string' && name.length > 0 && name.length <= 120
  && !['__proto__', 'constructor', 'prototype'].includes(name) && /^[A-Za-z0-9_.:$[\]-]+$/.test(name);
export function rustRegexCompatible(pattern) {
  if (typeof pattern !== 'string' || /\(\?<?[=!]|\\[1-9]|\\k</.test(pattern)) return false;
  try { new RegExp(pattern); return true; } catch { return false; }
}
function checkKeywords(schema, at) {
  for (const key of Object.keys(schema)) if (!SUPPORTED.has(key) && !ANNOTATIONS.has(key)) schemaFail(`${at}: unsupported keyword ${key}`);
  if ('pattern' in schema && !rustRegexCompatible(schema.pattern)) schemaFail(`${at}: pattern`);
}
/** external.rs checked_schema: annotation-only keywords removed, anything else must be checkable. */
export function checkedSchema(value, depth = 0, at = '$') {
  if (depth > 6) schemaFail(`${at}: too deep`);
  if (!isObject(value)) schemaFail(`${at}: not an object`);
  const v = structuredClone(value);
  for (const key of STRIPPED) delete v[key];
  if ('const' in v) {
    const constant = v.const; delete v.const;
    if ('enum' in v && !Array.isArray(v.enum)) schemaFail(`${at}: enum`);
    if (Array.isArray(v.enum) && !v.enum.some(x => JSON.stringify(x) === JSON.stringify(constant))) schemaFail(`${at}: const outside enum`);
    v.enum = [constant];
  }
  if ('type' in v) { if (typeof v.type !== 'string' || !TYPES.has(v.type)) schemaFail(`${at}: type`); }
  else if (!('enum' in v)) schemaFail(`${at}: untyped`);
  if ('properties' in v) {
    if (!isObject(v.properties) || Object.keys(v.properties).length > 64) schemaFail(`${at}: properties`);
    for (const [name, child] of Object.entries(v.properties)) {
      if (!validName(name)) schemaFail(`${at}: property name ${name}`);
      v.properties[name] = checkedSchema(child, depth + 1, `${at}.${name}`);
    }
  }
  if ('items' in v) v.items = checkedSchema(v.items, depth + 1, `${at}[*]`);
  if (('additionalProperties' in v && typeof v.additionalProperties !== 'boolean')
    || ('enum' in v && !(Array.isArray(v.enum) && v.enum.length))
    || ['minimum', 'maximum'].some(k => k in v && typeof v[k] !== 'number')
    || ['minLength', 'maxLength'].some(k => k in v && !(Number.isInteger(v[k]) && v[k] >= 0))) schemaFail(`${at}: constraint`);
  if ('required' in v) {
    if (!Array.isArray(v.required) || !isObject(v.properties)) schemaFail(`${at}: required`);
    if (v.required.some(n => typeof n !== 'string' || !Object.hasOwn(v.properties, n))) schemaFail(`${at}: required names`);
  }
  checkKeywords(v, at);
  return v;
}
/** Minimal instance check used for the declared `method`/`type` transport fields. */
function accepts(schema, value) {
  if (Array.isArray(schema.enum) && !schema.enum.includes(value)) return false;
  return !schema.type || schema.type === typeof value;
}
const GROUPS = { pathParams: 'path', queryParams: 'query', body: 'body', bodyFields: 'body' };
/**
 * The declared call schema, unwrapped from `extensions.bazaar.schema.properties.input`
 * (legacy-executor sources.rs declared_call_shape) and split into locations.
 * One adaptation of external.rs fields(): `bodyType` (JSON only) and optional-only
 * `headers` are transport metadata like `method`/`type` and are not provider inputs.
 */
export function declaredSchema(item, method) {
  let input = item.extensions?.bazaar?.schema?.properties?.input;
  if (input == null) return { error: 'missing_input_schema' };
  if (!isObject(input)) return { error: 'schema_unsupported', detail: 'input schema is not an object' };
  if (JSON.stringify(input).length > MAX_SCHEMA_BYTES) return { error: 'schema_unsupported', detail: 'input schema too large' };
  // Headers are never forwarded as provider inputs: a required one cannot be
  // supplied, an optional group is dropped before the schema is checked.
  const headers = isObject(input.properties) ? input.properties.headers : undefined;
  if (headers !== undefined) {
    if (!isObject(headers) || (Array.isArray(headers.required) && headers.required.length)
      || (Array.isArray(input.required) && input.required.includes('headers'))) return { error: 'required_headers' };
    const { headers: _dropped, ...rest } = input.properties;
    input = { ...input, properties: rest };
  }
  let root;
  try { root = checkedSchema(input); } catch (e) { if (e instanceof SchemaError) return { error: 'schema_unsupported', detail: e.message }; throw e; }
  if (root.type !== 'object' || !isObject(root.properties ?? {})) return { error: 'schema_unsupported', detail: 'root is not an object schema' };
  const properties = root.properties ?? {};
  const keys = Object.keys(properties);
  const grouped = keys.some(k => k in GROUPS) || keys.includes('method') || keys.includes('type');
  if (!grouped) return { error: 'schema_unsupported', detail: 'flat input schema without method' };
  for (const key of Object.keys(root)) {
    if (!['type', 'properties', 'required', 'additionalProperties', 'title', 'description'].includes(key)) return { error: 'schema_unsupported', detail: `root keyword ${key}` };
  }
  const groups = {};
  for (const [name, schema] of Object.entries(properties)) {
    if (name in GROUPS) {
      if (groups[GROUPS[name]]) return { error: 'schema_unsupported', detail: `duplicate ${GROUPS[name]} group` };
      groups[GROUPS[name]] = schema; continue;
    }
    if (name === 'method') { if (!accepts(schema, method)) return { error: 'schema_unsupported', detail: 'declared method schema excludes the method' }; continue; }
    if (name === 'type') { if (!accepts(schema, 'http')) return { error: 'schema_unsupported', detail: 'type is not http' }; continue; }
    if (name === 'bodyType') { if (!accepts(schema, 'json')) return { error: 'body_type_unsupported' }; continue; }
    return { error: 'schema_unsupported', detail: `input group ${name}` };
  }
  const infoBodyType = item.extensions?.bazaar?.info?.input?.bodyType;
  if (infoBodyType != null && infoBodyType !== 'json') return { error: 'body_type_unsupported' };
  if (groups.path) return { error: 'path_parameters' };
  if (method === 'GET' && groups.body) return { error: 'schema_unsupported', detail: 'GET with a body' };
  const out = {};
  let count = 0;
  for (const [location, group] of Object.entries(groups)) {
    if (group.type !== 'object' || (group.properties != null && !isObject(group.properties))) return { error: 'schema_unsupported', detail: `${location} group is not an object` };
    const props = group.properties ?? {};
    if (location === 'body' && group.properties == null) return { error: 'schema_unsupported', detail: 'free-form body' };
    count += Object.keys(props).length;
    const shape = { type: 'object', properties: props };
    if (Array.isArray(group.required) && group.required.length) shape.required = group.required;
    if (typeof group.additionalProperties === 'boolean') shape.additionalProperties = group.additionalProperties;
    if (typeof group.description === 'string') shape.description = group.description;
    out[location] = shape;
  }
  if (count > 64) return { error: 'schema_unsupported', detail: 'more than 64 inputs' };
  return { query_params: out.query ?? null, request_body: out.body ?? null, input_count: count };
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------
const EFFECT = /\b(create|delete|send|publish|transfer|order|subscribe|unsubscribe|render|upload|execute|deploy)\b/i; // source-operation-contracts.mjs
const SPECIALIZED = /exploit|penetration|pentest|intrusion|payload|offensive|vulnerability.?scan|attack.surface|open ports|credential.dump|initial.access.broker/i;
const ASYNC = /pending job|asynchronous|async job/i;
export function actionType(description, pathname) {
  const text = `${pathname} ${description}`.toLowerCase();
  if (/\b(generat\w*|image|video|audio|speech|tts|transcri\w*|translat\w*|summar\w*|rewrite|completion|llm|prompt)\b/.test(text)) return 'generate';
  if (/\b(search\w*|find|discover\w*|query|list\w*|browse|explore|scan\w*)\b/.test(text)) return 'search';
  if (/\b(scor\w*|analy[sz]\w*|calculat\w*|estimat\w*|predict\w*|classif\w*|convert\w*|verif\w*|validat\w*|screen\w*|check\w*|detect\w*|compar\w*|simulat\w*|audit\w*|assess\w*|risk)\b/.test(text)) return 'compute';
  return 'lookup';
}
export function endpointCategory(text) {
  const t = String(text).toLowerCase();
  if (/\b(crypto\w*|defi|tokens?|wallets?|onchain|on-chain|blockchain|chain|markets?|prices?|stocks?|financ\w*|trading|swaps?|nfts?|ethereum|eth|solana|bitcoin|btc|forex|dex)\b/.test(t)) return 'finance';
  if (/\b(images?|video|audio|media|music|photos?|speech|tts)\b/.test(t)) return 'media';
  if (/\b(weather|geo\w*|location|maps?|address\w*|places?|countr\w*)\b/.test(t)) return 'location';
  if (/\b(llm|inference|models?|gpu|compute)\b/.test(t)) return 'compute';
  return 'data';
}

// ---------------------------------------------------------------------------
// Admission: one listing -> a candidate endpoint, or the first reason it is not
// ---------------------------------------------------------------------------
const exclude = (reason, detail) => ({ ok: false, reason, detail });
function chooseAccept(item) {
  const all = Array.isArray(item.accepts) ? item.accepts.filter(isObject) : [];
  const exact = all.filter(a => a.scheme === 'exact');
  const base = exact.filter(a => a.network === BASE_NETWORK);
  const usdc = base.filter(a => String(a.asset ?? '').toLowerCase() === BASE_USDC.toLowerCase());
  if (!usdc.length) {
    const rails = [...new Set(all.map(a => `${a.scheme}@${a.network}`))].sort().join(',');
    return exclude(!exact.length ? 'no_exact_scheme' : !base.length ? 'no_base_network' : 'no_base_usdc_asset', rails.slice(0, 200));
  }
  const terms = new Map();
  for (const a of usdc) {
    const amount = a.amount ?? a.maxAmountRequired;
    if (a.amount != null && a.maxAmountRequired != null && String(a.amount) !== String(a.maxAmountRequired)) return exclude('ambiguous_amount');
    if (!/^\d{1,18}$/.test(String(amount ?? ''))) return exclude('unpriced', 'no integer amount');
    if (!/^0x[0-9a-fA-F]{40}$/.test(String(a.payTo ?? ''))) return exclude('invalid_pay_to');
    terms.set(`${BigInt(amount)}:${a.payTo.toLowerCase()}`, a);
  }
  if (terms.size > 1) return exclude('ambiguous_offer', `${terms.size} Base USDC terms`);
  const [accept] = terms.values();
  return { ok: true, accept, amount: BigInt(accept.amount ?? accept.maxAmountRequired), payTo: accept.payTo.toLowerCase() };
}
function declaredMethod(item) {
  const bazaar = item.extensions?.bazaar;
  const info = bazaar?.info?.input?.method;
  const schema = bazaar?.schema?.properties?.input?.properties?.method;
  const candidates = [info, schema?.const, ...(Array.isArray(schema?.enum) && schema.enum.length === 1 ? schema.enum : [])];
  const method = candidates.find(m => typeof m === 'string');
  return method ? method.toUpperCase() : null;
}
/**
 * @param ctx {now, existingHosts:Set, deniedHosts:Set, hostMaxAtomic:Map, minScore}
 */
export function admitItem(item, ctx) {
  const now = ctx.now ?? Date.now();
  if (!isObject(item)) return exclude('malformed');
  if (item.type !== 'http') return exclude('not_http', String(item.type));
  let url;
  try { url = new URL(String(item.resource ?? '')); } catch { return exclude('invalid_resource'); }
  if (url.protocol !== 'https:' || url.username || url.password || url.hash || (url.port && url.port !== '443') || String(item.resource).length > 2048) return exclude('unsafe_resource_url');
  const host = url.hostname.toLowerCase();
  if (isUnder(host, 'apiosk.com')) return exclude('apiosk_host');
  if (unsafeHost(host)) return exclude('unsafe_host', host);
  if (url.search) return exclude('resource_has_query');
  const serviceName = typeof item.serviceName === 'string' && item.serviceName.trim() ? item.serviceName.trim() : null;
  if (paasHost(host) && !serviceName) return exclude('anonymous_paas_host', host);
  const chosen = chooseAccept(item);
  if (!chosen.ok) return chosen;
  if (chosen.amount <= 0n) return exclude('unpriced', 'zero amount');
  if (chosen.amount > MAX_PRICE_ATOMIC) return exclude('price_above_cap', atomicToUsd(chosen.amount));
  if (![1, 2].includes(item.x402Version ?? chosen.accept.x402Version ?? 2)) return exclude('x402_version_unsupported');
  const method = declaredMethod(item);
  if (!method) return exclude('method_undeclared');
  if (!['GET', 'POST'].includes(method)) return exclude('method_unsupported', method);
  const q = item.quality;
  const calls = Number.isFinite(q?.l30DaysTotalCalls) ? q.l30DaysTotalCalls : 0;
  if (calls <= 0) return exclude('no_recent_calls');
  const age = daysSince(item.lastUpdated, now);
  if (age == null || age > STALE_DAYS) return exclude('stale_listing', item.lastUpdated ?? 'no lastUpdated');
  const description = String(item.description ?? '').replace(/\s+/g, ' ').trim();
  if (!description) return exclude('missing_description');
  if (looksLikeToy(item.resource) || looksLikeToy(description)) return exclude('toy');
  if (junkDescription(description)) return exclude('junk_description');
  const pathname = url.pathname || '/';
  const decoded = pathname.replaceAll('%7B', '{').replaceAll('%7D', '}').replaceAll('%7b', '{').replaceAll('%7d', '}');
  const segments = decoded.split('/');
  const placeholder = segments.findIndex(s => s.startsWith(':') || s.includes('{') || s.includes('}') || s === '*');
  if (placeholder >= 0) return exclude(placeholder === segments.length - 1 ? 'path_placeholder_trailing' : 'path_placeholder_mid', pathname);
  if (item.extensions?.bazaar?.routeTemplate && item.extensions.bazaar.routeTemplate !== pathname) return exclude('path_placeholder_mid', 'routeTemplate');
  if (EFFECT.test(pathname)) return exclude('effect_review_required', pathname);
  if (SPECIALIZED.test(`${pathname} ${description}`)) return exclude('specialized_review_required');
  if (ASYNC.test(description)) return exclude('asynchronous_result');
  const schema = declaredSchema(item, method);
  if (schema.error) return exclude(schema.error, schema.detail);
  const score = scoreListing(item, chosen.accept, now);
  if (score.score < (ctx.minScore ?? MIN_LISTING_SCORE)) return exclude('low_listing_score', `${score.score}/10${score.capped ? ` (${score.capped})` : ''}`);
  if (ctx.existingHosts?.has(host)) return exclude('host_already_listed', host);
  if (ctx.deniedHosts?.has(host)) return exclude('host_policy_deny', host);
  const hostMax = ctx.hostMaxAtomic?.get(host);
  if (hostMax != null && chosen.amount > hostMax) return exclude('host_policy_price_cap', host);
  const output = item.extensions?.bazaar?.info?.output?.example;
  const example = (isObject(output) || Array.isArray(output)) && JSON.stringify(output).length <= MAX_EXAMPLE_BYTES ? output : null;
  const bazaarTags = Array.isArray(item.extensions?.bazaar?.tags) ? item.extensions.bazaar.tags : [];
  const tags = [...new Set([...(Array.isArray(item.tags) ? item.tags : []), ...bazaarTags]
    .filter(t => typeof t === 'string').map(t => t.trim().toLowerCase()).filter(t => t && t.length <= 80))].slice(0, 8);
  const icon = typeof item.iconUrl === 'string' && /^https:\/\/[^\s]+$/.test(item.iconUrl) && item.iconUrl.length <= 2048 ? item.iconUrl : null;
  const categoryText = `${item.extensions?.bazaar?.category ?? ''} ${tags.join(' ')} ${description}`;
  return { ok: true, candidate: {
    resource: `https://${url.host}${pathname}`, host, origin: `https://${url.host}`, method, path: pathname,
    service_name: serviceName, icon_url: icon, tags, description: description.slice(0, 2000),
    price_atomic: chosen.amount.toString(), price_usd: atomicToUsd(chosen.amount), pay_to: chosen.payTo, network: BASE_NETWORK,
    query_params: schema.query_params, request_body: schema.request_body, input_count: schema.input_count,
    response_example: example, action_type: actionType(description, pathname), endpoint_category: endpointCategory(categoryText),
    calls_30d: calls, payers_30d: Number.isFinite(q?.l30DaysUniquePayers) ? q.l30DaysUniquePayers : 0,
    last_called_at: q?.lastCalledAt ?? null, last_updated: item.lastUpdated, score: score.score,
  } };
}

// ---------------------------------------------------------------------------
// Catalog: admitted candidates -> ranked tier -> services with staged contracts
// ---------------------------------------------------------------------------
const rank = (a, b) => b.calls_30d - a.calls_30d || b.payers_30d - a.payers_30d || a.resource.localeCompare(b.resource) || a.method.localeCompare(b.method);
export const resourceKey = c => `${c.host} ${c.method} ${c.path}`;
/** Identifier-like segments (UA123, KJFK, 0x…, 42) become `*`; words and versions stay. */
export const pathShape = pathname => pathname.split('/').map(seg =>
  /^\d+$/.test(seg) || /^0x[0-9a-f]{6,}$/i.test(seg) || /^[A-Z]{3,4}$/.test(seg) || (/^[A-Z0-9]{2,}$/.test(seg) && /\d/.test(seg)) ? '*' : seg).join('/');
export function contentHash(c, service) {
  return digest({ method: c.method, path: c.path, price_atomic: c.price_atomic, pay_to: c.pay_to, description: c.description,
    query_params: c.query_params, request_body: c.request_body, response_example: c.response_example, action_type: c.action_type,
    endpoint_category: c.endpoint_category, tags: c.tags, service: { name: service.name, logo: service.logo } });
}

/**
 * items -> {admitted, excluded, tier, services, contracts}. Pure: no I/O.
 * opts: {tier, maxPerHost, now, existingHosts, deniedHosts, hostMaxAtomic, minScore}
 */
export function buildCatalog(items, opts = {}) {
  const tierSize = opts.tier ?? 500;
  const excluded = [];
  const admitted = [];
  for (const item of items) {
    const result = admitItem(item, opts);
    if (result.ok) admitted.push(result.candidate);
    else excluded.push({ resource: String(item?.resource ?? '').slice(0, 300), method: declaredMethodSafe(item), reason: result.reason, detail: result.detail ?? null });
  }
  admitted.sort(rank);
  // A listing per concrete call (/flights/UA123, /flights/AA95) is one example
  // of a template, not an endpoint. Two or more on a host with the same shape
  // are all excluded; the template itself is not published.
  const shapes = Map.groupBy(admitted, c => `${c.host} ${c.method} ${pathShape(c.path)}`);
  const instances = new Set([...shapes].filter(([key, rows]) => key.includes('*') && rows.length > 1).flatMap(([, rows]) => rows));
  const seen = new Set();
  const unique = [];
  for (const c of admitted) {
    if (instances.has(c)) { excluded.push({ resource: c.resource, method: c.method, reason: 'concrete_instance_path', detail: pathShape(c.path) }); continue; }
    if (seen.has(resourceKey(c))) { excluded.push({ resource: c.resource, method: c.method, reason: 'duplicate_resource', detail: null }); continue; }
    seen.add(resourceKey(c)); unique.push(c);
  }
  const perHost = new Map();
  const tier = [];
  const beyondHostCap = [];
  for (const c of unique) {
    if (tier.length >= tierSize) break;
    const n = perHost.get(c.host) ?? 0;
    if (n >= (opts.maxPerHost ?? MAX_PER_HOST)) { beyondHostCap.push(c); continue; }
    perHost.set(c.host, n + 1); tier.push(c);
  }
  const byHost = Map.groupBy(tier, c => c.host);
  const services = [...byHost].map(([host, rows]) => {
    const names = new Map();
    for (const r of rows) if (r.service_name) names.set(r.service_name, (names.get(r.service_name) ?? 0) + 1);
    const name = ([...names].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))[0]?.[0] ?? host).slice(0, 160);
    const top = rows[0];
    const categories = Map.groupBy(rows, r => r.endpoint_category);
    const service = {
      id: uuid(['bazaar-api-v1', host]), slug: serviceSlug(host), host, name, origin_url: top.origin, website_url: top.origin,
      logo: rows.find(r => r.icon_url)?.icon_url ?? null,
      category: [...categories].sort((a, b) => b[1].length - a[1].length || a[0].localeCompare(b[0]))[0][0],
      description: `Pay-per-call x402 service at ${host}, listed in the Coinbase Bazaar. ${top.description}`.slice(0, 600),
      tags: [...new Set(rows.flatMap(r => r.tags))].slice(0, 16),
      pay_to: [...new Set(rows.map(r => r.pay_to))].sort(),
      calls_30d: rows.reduce((n, r) => n + r.calls_30d, 0),
    };
    service.endpoints = rows.map(r => ({ ...r, id: uuid(['bazaar-endpoint-v1', host, r.method, r.path]) }));
    for (const e of service.endpoints) e.content_hash = contentHash(e, service);
    return service;
  });
  // Staged contracts through the provider-neutral document adapter, exactly as
  // every other declared source: unverified until a first usable live response.
  const snapshot = {
    apis: services.map(s => ({ id: s.id, slug: s.slug, name: s.name, docs_url: s.website_url, listing_type: 'federated' })),
    endpoints: services.flatMap(s => s.endpoints.map(e => ({ id: e.id, api_id: s.id, method: e.method, path: e.path, price: Number(e.price_usd),
      description: e.description, query_params: e.query_params, request_body: e.request_body, contract_documented: true, action_type: e.action_type }))),
    offers: services.flatMap(s => s.endpoints.map(e => ({ endpoint_id: e.id, source: 'declared' }))),
    contracts: [],
  };
  const contracts = buildOperations(snapshot);
  return { admitted: unique, excluded, tier, beyondHostCap, services, contracts };
}
const declaredMethodSafe = item => { try { return declaredMethod(item); } catch { return null; } };

// ---------------------------------------------------------------------------
// Batches: <= 200 endpoint rows and <= 280 KB per RPC call / SQL chunk
// ---------------------------------------------------------------------------
export function batchRows(catalog) {
  const contractFor = new Map(catalog.contracts.services.map(s => [s.endpoint_id, s]));
  const report = new Map(catalog.contracts.report.map(r => [r.endpoint_id, r]));
  const words = new Map(catalog.contracts.vocabulary.map(w => [w.slug, w]));
  const rows = [];
  for (const s of catalog.services) {
    const service = { slug: s.slug, name: s.name, description: s.description, origin_url: s.origin_url, website_url: s.website_url,
      logo: s.logo, category: s.category, tags: s.tags, pay_to: s.pay_to };
    for (const e of s.endpoints) {
      const staged = contractFor.get(e.id);
      const contract = staged && {
        id: staged.id, capability_slug: staged.contract.capability_slug, description: staged.description,
        requires: staged.contract.requires, input_map: staged.contract.input_map, output_map: staged.contract.output_map,
        produces: Object.keys(staged.contract.output_map.produces).map(fact_type => ({ fact_type, subject_role: 'subject', cardinality: staged.contract.cardinality })),
        omitted_optional_inputs: report.get(e.id)?.omitted_optional_inputs ?? [],
        vocabulary: [...staged.contract.requires.map(r => r.fact_type), ...Object.keys(staged.contract.output_map.produces)]
          .map(slug => words.get(slug)).filter(Boolean)
          .map(w => ({ slug: w.slug, description: String(w.description).slice(0, 500), subject_kind: w.subject_kind, value_schema: w.value_schema })),
      };
      rows.push({ host: s.host, method: e.method, path: e.path, resource: e.resource, content_hash: e.content_hash, service,
        endpoint: { id: e.id, price_usd: e.price_usd, pay_to: e.pay_to, description: e.description, query_params: e.query_params,
          request_body: e.request_body, response_example: e.response_example, action_type: e.action_type, endpoint_category: e.endpoint_category,
          tags: e.tags, calls_30d: e.calls_30d, payers_30d: e.payers_30d },
        contract: contract || null });
    }
  }
  return rows;
}
export function batches(rows, maxRows = BATCH_MAX_ROWS, maxBytes = BATCH_MAX_BYTES) {
  const out = [];
  let current = [], bytes = 0;
  for (const row of rows) {
    const size = Buffer.byteLength(JSON.stringify(row));
    if (size > maxBytes) throw new Error(`One row exceeds the batch byte limit: ${row.resource}`);
    if (current.length && (current.length >= maxRows || bytes + size > maxBytes)) { out.push(current); current = []; bytes = 0; }
    current.push(row); bytes += size;
  }
  if (current.length) out.push(current);
  return out;
}
/** One file per batch: a single RPC call, reviewable and replayable by hand. */
export function sqlChunk(batch, index, total) {
  const json = JSON.stringify({ run_id: null, rows: batch });
  let tag = 'bazaar';
  while (json.includes(`$${tag}$`)) tag += 'x';
  const endpoints = batch.length, hosts = new Set(batch.map(r => r.host)).size;
  return `-- Coinbase Bazaar import, batch ${index + 1}/${total}: ${hosts} services, ${endpoints} endpoints.\n`
    + `-- Generated by scripts/bazaar-sync.mjs (dry run). Needs migration coinbase_bazaar_source, the kill switch ON\n`
    + `-- and one running providers.bazaar_sync_runs row; the RPC merges, never deletes.\n`
    + `SELECT providers.upsert_bazaar_batch($${tag}$${json}$${tag}$::jsonb);\n`;
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------
const median = xs => { const s = [...xs].sort((a, b) => a - b); return s.length ? s[Math.floor((s.length - 1) / 2)] : null; };
export function summarize({ read, total, pages, complete }, catalog, rows, chunks, tierSize) {
  const reasons = {};
  for (const x of catalog.excluded) reasons[x.reason] = (reasons[x.reason] ?? 0) + 1;
  const sortedReasons = Object.fromEntries(Object.entries(reasons).sort((a, b) => b[1] - a[1]));
  const prices = catalog.tier.map(c => Number(c.price_atomic));
  const blockers = {};
  for (const r of catalog.contracts.report) for (const b of r.blockers) blockers[b] = (blockers[b] ?? 0) + 1;
  const count = (xs, f) => Object.fromEntries(Object.entries(Object.groupBy(xs, f)).map(([k, v]) => [k, v.length]).sort((a, b) => b[1] - a[1]));
  return {
    source: BAZAAR_URL, read_count: read, source_total: total, pages, complete_read: complete,
    admitted_count: catalog.admitted.length, excluded_count: catalog.excluded.length,
    excluded_by_reason: sortedReasons,
    admitted_hosts: new Set(catalog.admitted.map(c => c.host)).size,
    pilot: {
      tier: tierSize, resources: catalog.tier.length, hosts: catalog.services.length,
      beyond_host_cap: catalog.beyondHostCap.length,
      price_usd: { min: prices.length ? atomicToUsd(Math.min(...prices)) : null, median: prices.length ? atomicToUsd(median(prices)) : null, max: prices.length ? atomicToUsd(Math.max(...prices)) : null },
      networks: count(catalog.tier, c => c.network), assets: { [BASE_USDC]: catalog.tier.length },
      methods: count(catalog.tier, c => c.method), action_types: count(catalog.tier, c => c.action_type), categories: count(catalog.tier, c => c.endpoint_category),
      calls_30d: catalog.tier.reduce((n, c) => n + c.calls_30d, 0),
      staged_contracts: catalog.contracts.services.length, listing_only_endpoints: catalog.tier.length - catalog.contracts.services.length,
      contract_blockers: blockers,
      top_hosts: catalog.services.toSorted((a, b) => b.calls_30d - a.calls_30d).slice(0, 25)
        .map(s => ({ host: s.host, slug: s.slug, name: s.name, endpoints: s.endpoints.length, calls_30d: s.calls_30d, pay_to: s.pay_to.length })),
    },
    batches: { count: chunks.length, rows: rows.length, max_rows: Math.max(0, ...chunks.map(b => b.length)),
      max_bytes: Math.max(0, ...chunks.map(b => Buffer.byteLength(JSON.stringify({ run_id: null, rows: b })))) },
  };
}

// ---------------------------------------------------------------------------
// I/O: polite paging of the public discovery API
// ---------------------------------------------------------------------------
export async function fetchAll({ fetchImpl = fetch, log = () => {}, gapMs = PAGE_GAP_MS, maxPages = MAX_PAGES, retryMs = 2000 } = {}) {
  const items = [];
  let total = null, offset = 0, pages = 0;
  while (pages < maxPages) {
    let body;
    for (let attempt = 0; ; attempt++) {
      const url = `${BAZAAR_URL}?limit=${PAGE_LIMIT}&offset=${offset}`;
      const response = await fetchImpl(url, { headers: { accept: 'application/json', 'user-agent': 'apiosk-bazaar-sync/1 (+https://apiosk.com)' }, signal: AbortSignal.timeout(60_000) });
      if (response.ok) { body = await response.json(); break; }
      if (attempt >= 3 || ![429, 500, 502, 503, 504].includes(response.status)) throw new Error(`Bazaar HTTP ${response.status} at offset ${offset}`);
      await sleep(Math.min(30_000, retryMs * 2 ** attempt));
    }
    pages++;
    const page = Array.isArray(body?.items) ? body.items : [];
    total = Number.isInteger(body?.pagination?.total) ? body.pagination.total : total;
    items.push(...page);
    log(`page ${pages}: offset ${offset}, ${page.length} items, total ${total}`);
    offset += page.length;
    if (!page.length || (total != null && offset >= total)) break;
    await sleep(gapMs);
  }
  return { items, total, pages, complete: total != null && items.length >= total };
}

// ---------------------------------------------------------------------------
// --apply: operator path. Never run from the dry run.
// ---------------------------------------------------------------------------
async function rest(env, method, route, { body, profile = 'providers', timeoutMs = 10_000 } = {}) {
  const started = Date.now();
  const response = await fetch(new URL(`/rest/v1/${route}`, env.SUPABASE_URL), {
    method, body: body && JSON.stringify(body), signal: AbortSignal.timeout(timeoutMs),
    headers: { apikey: env.SUPABASE_SERVICE_ROLE_KEY, authorization: `Bearer ${env.SUPABASE_SERVICE_ROLE_KEY}`, 'content-type': 'application/json',
      [method === 'GET' ? 'accept-profile' : 'content-profile']: profile },
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`${method} ${route}: HTTP ${response.status} ${text.slice(0, 300)}`);
  return { data: text ? JSON.parse(text) : null, ms: Date.now() - started };
}
async function healthy(env) {
  const started = Date.now();
  try {
    const response = await fetch(env.GATEWAY_HEALTH_URL ?? 'https://gateway.apiosk.com/health', { signal: AbortSignal.timeout(5000) });
    return response.ok && Date.now() - started < 3000;
  } catch { return false; }
}
async function killSwitch(env) {
  const { data } = await rest(env, 'GET', 'bazaar_sync_control?select=enabled,max_tier&id=eq.true');
  return data?.[0] ?? { enabled: false, max_tier: 0 };
}
export async function apply(catalog, rows, read, { env = process.env, tier, log = console.error } = {}) {
  for (const key of ['SUPABASE_URL', 'SUPABASE_SERVICE_ROLE_KEY']) if (!env[key]) throw new Error(`--apply needs ${key}`);
  const control = await killSwitch(env);
  if (!control.enabled) throw new Error('Kill switch is OFF (providers.bazaar_sync_control.enabled=false); nothing written.');
  if (tier > control.max_tier) throw new Error(`Tier ${tier} exceeds the approved ceiling ${control.max_tier}.`);
  if (!await healthy(env)) throw new Error('Gateway /health is not healthy; not starting.');
  const tracked = [];
  for (let offset = 0; ; offset += 1000) {
    const { data } = await rest(env, 'GET', `bazaar_resources?select=host,method,path,content_hash,retired_at&order=host,method,path&limit=1000&offset=${offset}`);
    tracked.push(...data);
    if (data.length < 1000) break;
    await sleep(500);
  }
  const known = new Map(tracked.map(t => [`${t.host} ${t.method} ${t.path}`, t]));
  const changed = rows.filter(r => { const t = known.get(`${r.host} ${r.method} ${r.path}`); return !t || t.retired_at || t.content_hash !== r.content_hash; });
  const changedSet = new Set(changed);
  const touched = rows.filter(r => !changedSet.has(r)).map(r => ({ host: r.host, method: r.method, path: r.path }));
  const { data: run } = await rest(env, 'POST', 'rpc/begin_bazaar_sync_run', { body: { p_tier: tier, p_note: `bazaar-sync.mjs: ${changed.length} changed, ${touched.length} unchanged` } });
  log(`run ${run}: ${changed.length} changed rows, ${touched.length} unchanged`);
  const calls = [...batches(changed).map(b => ({ run_id: run, rows: b })), ...chunk(touched, BATCH_MAX_ROWS).map(t => ({ run_id: run, rows: [], touch: t }))];
  const finish = async (complete, note) => (await rest(env, 'POST', 'rpc/finish_bazaar_sync_run', { body: { p_run: run, p_result: {
    complete, note, missed: complete ? missedKeys(tracked, catalog.admitted) : [],
    report: { read_count: read.read, source_total: read.total, admitted: catalog.admitted.length, tier: catalog.tier.length } } }, timeoutMs: 15_000 })).data;
  try {
    for (const [i, batch] of calls.entries()) {
      if (i) await sleep(2000);
      if (!(await killSwitch(env)).enabled) return { run, aborted: 'kill_switch', result: await finish(false, 'kill switch turned off') };
      const { data, ms } = await rest(env, 'POST', 'rpc/upsert_bazaar_batch', { body: { p_batch: batch }, timeoutMs: 20_000 });
      log(`batch ${i + 1}/${calls.length}: ${ms} ms ${JSON.stringify(data).slice(0, 300)}`);
      if (ms > 5000) return { run, aborted: 'slow_rpc', result: await finish(false, `batch ${i + 1} took ${ms} ms`) };
      if (!await healthy(env)) return { run, aborted: 'gateway_unhealthy', result: await finish(false, `gateway degraded after batch ${i + 1}`) };
    }
    return { run, result: await finish(read.complete, read.complete ? 'complete' : 'incomplete Bazaar read: no retirement') };
  } catch (error) {
    await finish(false, `failed: ${String(error.message).slice(0, 200)}`).catch(() => {});
    throw error;
  }
}
const chunk = (xs, n) => Array.from({ length: Math.ceil(xs.length / n) }, (_, i) => xs.slice(i * n, i * n + n));
/** Tracked, unretired resources that are no longer admitted anywhere in the full read. */
export function missedKeys(tracked, admitted) {
  const live = new Set(admitted.map(resourceKey));
  return tracked.filter(t => !t.retired_at && !live.has(`${t.host} ${t.method} ${t.path}`)).map(t => ({ host: t.host, method: t.method, path: t.path }));
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------
function args(argv) {
  const out = { tier: 500, apply: false, input: [] };
  for (const arg of argv) {
    const [key, value] = arg.replace(/^--/, '').split(/=(.*)/s);
    if (key === 'apply') out.apply = true;
    else if (key === 'input') out.input.push(value);
    else if (['tier', 'min-score', 'max-per-host'].includes(key)) { out[key] = Number(value); if (!Number.isInteger(out[key]) || out[key] < 0) throw new Error(`--${key} must be a whole number`); }
    else if (['out', 'existing', 'host-policy'].includes(key)) out[key] = value;
    else throw new Error(`Unknown argument ${arg}`);
  }
  if (out.tier < 1 || out.tier > 20_000) throw new Error('--tier must be between 1 and 20000');
  return out;
}
async function existingHosts(file) {
  if (!file) return new Set();
  const snapshot = JSON.parse(await fs.readFile(file, 'utf8'));
  const hosts = new Set();
  for (const a of snapshot.apis ?? []) {
    if (String(a.slug ?? '').startsWith(SERVICE_PREFIX)) continue;
    for (const u of [a.origin_url, a.website_url, a.endpoint_url]) { try { if (u) hosts.add(new URL(u).hostname.toLowerCase()); } catch { /* not a URL */ } }
  }
  return hosts;
}
async function hostPolicy(file) {
  const denied = new Set(), max = new Map();
  if (!file) return { denied, max };
  for (const row of JSON.parse(await fs.readFile(file, 'utf8'))) {
    const host = String(row.host).toLowerCase();
    if (row.policy === 'deny') denied.add(host);
    else if (row.max_price_usdc != null) max.set(host, BigInt(Math.round(Number(row.max_price_usdc) * 1e6)));
  }
  return { denied, max };
}
async function main() {
  const opts = args(process.argv.slice(2));
  const log = message => console.error(`[bazaar-sync] ${message}`);
  const stamp = new Date().toISOString().replace(/[:.]/g, '-');
  const out = path.resolve(opts.out ?? new URL(`../output/bazaar/${stamp}`, import.meta.url).pathname);
  let read;
  if (opts.input.length) {
    const pages = await Promise.all(opts.input.map(async f => JSON.parse(await fs.readFile(f, 'utf8'))));
    const items = pages.flatMap(p => p.items ?? []);
    read = { items, total: pages.at(-1)?.pagination?.total ?? items.length, pages: pages.length, complete: false };
  } else read = await fetchAll({ log });
  const policy = await hostPolicy(opts['host-policy']);
  const catalog = buildCatalog(read.items, { tier: opts.tier, maxPerHost: opts['max-per-host'], minScore: opts['min-score'],
    existingHosts: await existingHosts(opts.existing), deniedHosts: policy.denied, hostMaxAtomic: policy.max, now: Date.now() });
  const rows = batchRows(catalog);
  const chunks = batches(rows);
  const report = { generated_at: new Date().toISOString(), mode: opts.apply ? 'apply' : 'dry-run', existing_snapshot: opts.existing ?? null,
    host_policy_file: opts['host-policy'] ?? null,
    ...summarize({ read: read.items.length, total: read.total, pages: read.pages, complete: read.complete }, catalog, rows, chunks, opts.tier) };
  await fs.mkdir(path.join(out, 'sql'), { recursive: true });
  await fs.writeFile(path.join(out, 'report.json'), JSON.stringify(report, null, 2) + '\n');
  await fs.writeFile(path.join(out, 'excluded.json'), JSON.stringify(catalog.excluded) + '\n');
  await fs.writeFile(path.join(out, 'catalog.json'), JSON.stringify({ services: catalog.services, contracts: catalog.contracts }, null, 1) + '\n');
  for (const [i, batch] of chunks.entries()) await fs.writeFile(path.join(out, 'sql', `${String(i).padStart(3, '0')}.sql`), sqlChunk(batch, i, chunks.length));
  log(`wrote ${out}: ${catalog.admitted.length} admitted of ${read.items.length}; pilot ${catalog.tier.length} resources on ${catalog.services.length} hosts; ${chunks.length} batches`);
  if (opts.apply) {
    const result = await apply(catalog, rows, { read: read.items.length, total: read.total, complete: read.complete }, { tier: opts.tier, log });
    await fs.writeFile(path.join(out, 'apply.json'), JSON.stringify(result, null, 2) + '\n');
    log(`apply: ${JSON.stringify(result).slice(0, 500)}`);
  }
  console.log(JSON.stringify({ out, admitted: report.admitted_count, excluded: report.excluded_count, pilot: report.pilot.resources, hosts: report.pilot.hosts, batches: chunks.length }));
}
if (process.argv[1] && path.resolve(process.argv[1]) === new URL(import.meta.url).pathname) await main();
