#!/usr/bin/env node
// Public API.market category metadata only. No credentials, purchases, or DB writes.
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const ORIGIN = 'https://api.market';
const MAX_BYTES = 5 * 1024 * 1024;
const MAX_CATEGORIES = 64;
const GAP_MS = 1100; // robots.txt asks for at least one second between requests.
const SLUG = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const strings = value => Array.isArray(value) && value.every(item => typeof item === 'string');
const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));

/** Only the sitemap's canonical public category pages, never blogs or API routes. */
export function parseSitemap(xml) {
  const urls = [...xml.matchAll(/<loc>\s*([^<]+)\s*<\/loc>/g)].map(match => match[1].trim());
  const categories = [...new Set(urls.filter(url => /^https:\/\/api\.market\/category\/[a-z0-9-]+\/?$/.test(url)).map(url => url.replace(/\/$/, '')))];
  if (!categories.length || categories.length > MAX_CATEGORIES) throw new Error(`Unexpected category count: ${categories.length}`);
  return {
    categories: categories.sort(),
    products: [...new Set(urls.filter(url => /^https:\/\/api\.market\/store\/[^/?#]+\/[^/?#]+\/?$/.test(url)).map(url => url.replace(/\/$/, '')))].sort(),
  };
}

export function selectCategories(urls, requested = []) {
  if (!requested.length) return urls;
  const wanted = [...new Set(requested)];
  for (const slug of wanted) {
    if (!SLUG.test(slug) || !urls.includes(`${ORIGIN}/category/${slug}`)) throw new Error(`Unknown category: ${slug}`);
  }
  return urls.filter(url => wanted.includes(url.split('/').at(-1)));
}

function normalizeProduct(product, category, url) {
  if (!object(product) || typeof product.title !== 'string' || !product.title.trim()
    || typeof product.slug !== 'string' || !product.slug.trim()
    || !object(product.workspace) || typeof product.workspace.slug !== 'string' || !product.workspace.slug.trim()
    || !strings(product.categories) || !Array.isArray(product.pricingPlans)) throw new Error('Unexpected product metadata');
  const workspace = product.workspace.slug, slug = product.slug;
  if ([workspace, slug].some(value => /[\\/?#]/.test(value))) throw new Error('Unexpected product identity');
  // Encode segments from metadata; never let a vendor-supplied slug become a URL.
  const productUrl = `${ORIGIN}/store/${encodeURIComponent(workspace)}/${encodeURIComponent(slug)}`;
  return {
    id: `${workspace}/${slug}`, source: 'api.market', name: product.title,
    description: typeof product.subtitle === 'string' ? product.subtitle : '',
    workspace, slug, url: productUrl, provider_website: null,
    main_category: typeof product.mainCategory === 'string' ? product.mainCategory : null,
    categories: product.categories, tags: strings(product.metaTags) ? product.metaTags : [],
    review_status: typeof product.reviewStatus === 'string' ? product.reviewStatus : null,
    // These are published plan snapshots, not an executable per-call quote.
    pricing_plans: product.pricingPlans, reviews: Array.isArray(product.reviews) ? product.reviews : [],
    analytics: Array.isArray(product.analytics) ? product.analytics : [],
    discovered_in: [category], source_urls: [url], executable: false,
  };
}

/** Read structured data already delivered in the public category HTML, without executing it. */
export function parseCategoryPage(html, url) {
  const category = url.split('/').at(-1);
  if (!SLUG.test(category) || url !== `${ORIGIN}/category/${category}`) throw new Error('Not a public category URL');
  const match = html.match(/<script\b[^>]*\bid=["']__NEXT_DATA__["'][^>]*>([\s\S]*?)<\/script>/i);
  if (!match) throw new Error('Category has no structured catalog payload');
  const data = JSON.parse(match[1]), props = data?.props?.pageProps;
  if (data.isFallback || data.page !== '/category/[category]' || data.query?.category !== category
    || !object(props) || !Array.isArray(props.publicProducts) || typeof props.categoryName !== 'string') throw new Error('Unexpected category page shape');
  if (!Array.isArray(props.filterTags) || !props.filterTags.every(tag => object(tag) && typeof tag.value === 'string' && typeof tag.label === 'string')) throw new Error('Unexpected category filters');
  // Fail closed if the website introduces pagination. Silently reading page 1
  // must not turn a partial export into a supposedly complete category.
  const paginationKeys = Object.keys(props).filter(key => /cursor|offset|total|page|has.?more|limit|pagination/i.test(key));
  if (paginationKeys.length) throw new Error(`Category pagination needs review: ${paginationKeys.join(', ')}`);
  const products = props.publicProducts.map(product => normalizeProduct(product, category, url));
  return {
    slug: category, name: props.categoryName, url, subcategories: props.filterTags,
    count: products.length, unique_count: new Set(products.map(product => product.id)).size,
    build_id: typeof data.buildId === 'string' ? data.buildId : null,
    product_ids: products.map(product => product.id), products,
  };
}

export function buildCatalog({ pages, requestedUrls, sitemapProducts = [], failures = [], fetchedAt = new Date().toISOString() }) {
  const products = new Map(), duplicates = [];
  for (const page of pages) for (const product of page.products) {
    const previous = products.get(product.id);
    if (previous) {
      duplicates.push({ id: product.id, category: page.slug });
      previous.discovered_in = [...new Set([...previous.discovered_in, ...product.discovered_in])];
      previous.source_urls = [...new Set([...previous.source_urls, ...product.source_urls])];
    } else products.set(product.id, structuredClone(product));
  }
  const productList = [...products.values()].sort((a, b) => a.id.localeCompare(b.id));
  const productUrls = new Set(productList.map(product => product.url));
  const fetchedUrls = new Set(pages.map(page => page.url));
  return {
    categories: pages.map(({ products: _products, ...category }) => category),
    products: productList,
    report: {
      fetched_at: fetchedAt, source_sitemap: `${ORIGIN}/sitemap.xml`,
      requested_categories: requestedUrls.length, fetched_categories: pages.length,
      complete: failures.length === 0 && requestedUrls.every(url => fetchedUrls.has(url)),
      completeness_scope: 'All product records delivered by the selected public category pages; not a claim of every marketplace product.',
      pagination: 'Current category pages embed the full publicProducts array and the UI filters/sorts it locally. No total or cursor is published. New pagination fields stop the export for review.',
      unique_products: productList.length, category_memberships: pages.reduce((sum, page) => sum + page.count, 0),
      duplicate_memberships: duplicates, failures,
      sitemap_product_count: sitemapProducts.length,
      sitemap_products_not_in_selected_categories: sitemapProducts.filter(url => !productUrls.has(url)),
      category_products_not_in_sitemap: [...productUrls].filter(url => !sitemapProducts.includes(url)),
      metadata_only: true, executable: false,
      pricing_note: 'Published pricing-plan metadata only. Currency, current subscription eligibility and endpoint unit cost must be verified before execution.',
    },
  };
}

/** Fixed-origin, bounded GETs only, with no cookies or authorization headers. */
async function readPublicPage(url, fetchImpl) {
  if (url !== `${ORIGIN}/sitemap.xml` && !/^https:\/\/api\.market\/category\/[a-z0-9-]+$/.test(url)) throw new Error('Refusing non-catalog URL');
  const response = await fetchImpl(url, { method: 'GET', redirect: 'error', signal: AbortSignal.timeout(30_000), headers: { accept: 'text/html, application/xml', 'user-agent': 'ApioskCatalogReview/1.0 (+https://apiosk.com)' } });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  if (!response.body) throw new Error('Empty response');
  const chunks = []; let bytes = 0;
  for await (const chunk of response.body) {
    bytes += chunk.length;
    if (bytes > MAX_BYTES) throw new Error('Catalog response exceeds 5 MiB');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}

export async function fetchCatalog({ categories = [], fetchImpl = fetch, wait = sleep, onProgress = () => {} } = {}) {
  const sitemap = parseSitemap(await readPublicPage(`${ORIGIN}/sitemap.xml`, fetchImpl));
  const urls = selectCategories(sitemap.categories, categories), pages = [], failures = [];
  for (const url of urls) {
    await wait(GAP_MS);
    try {
      const page = parseCategoryPage(await readPublicPage(url, fetchImpl), url);
      pages.push(page); onProgress(`${page.slug}: ${page.count} APIs`);
    } catch (error) {
      failures.push({ url, error: error.message }); onProgress(`${url}: failed (${error.message})`);
    }
  }
  return buildCatalog({ pages, requestedUrls: urls, sitemapProducts: sitemap.products, failures });
}

export function parseArgs(args) {
  const options = { categories: [], out: path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../output/apimarket-catalog') };
  for (let index = 0; index < args.length; index++) {
    const [flag, ...valueParts] = args[index].split('=');
    if (flag === '--help') return { help: true };
    if (!['--category', '--out'].includes(flag)) throw new Error(`Unknown argument: ${flag}`);
    const value = valueParts.length ? valueParts.join('=') : args[++index];
    if (!value || value.startsWith('--')) throw new Error(`Missing value for ${flag}`);
    if (flag === '--category') options.categories.push(value);
    else options.out = path.resolve(value);
  }
  return options;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) { console.log('Usage: node scripts/apimarket-catalog.mjs [--category=slug ...] [--out=directory]\nFetches public category metadata only; never purchases or imports into production.'); return; }
  const result = await fetchCatalog({ ...options, onProgress: message => console.error(message) });
  await fs.mkdir(options.out, { recursive: true });
  for (const name of ['categories', 'products', 'report']) await fs.writeFile(path.join(options.out, `${name}.json`), `${JSON.stringify(result[name], null, 2)}\n`);
  const csvCell = value => {
    let text = String(value ?? '');
    if (/^[=+@\-\t\r]/.test(text)) text = `'${text}`;
    return `"${text.replaceAll('"', '""')}"`;
  };
  const categoryRows = [['slug', 'name', 'count', 'url'], ...result.categories.map(category => [category.slug, category.name, category.count, category.url])];
  await fs.writeFile(path.join(options.out, 'categories.csv'), `${categoryRows.map(row => row.map(csvCell).join(',')).join('\n')}\n`);
  console.log(JSON.stringify({ out: options.out, categories: result.categories.length, products: result.products.length, complete: result.report.complete }));
  if (!result.report.complete) process.exitCode = 1;
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
