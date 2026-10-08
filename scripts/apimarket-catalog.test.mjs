import test from 'node:test';
import assert from 'node:assert/strict';
import { buildCatalog, fetchCatalog, parseArgs, parseCategoryPage, parseSitemap, selectCategories } from './apimarket-catalog.mjs';

const url = slug => `https://api.market/category/${slug}`;
const sitemap = '<urlset><url><loc>https://api.market/category/data</loc></url><url><loc>https://api.market/category/finance</loc></url><url><loc>https://api.market/store/vendor/weather</loc></url></urlset>';
const product = { title: 'Weather API', subtitle: 'Current conditions', slug: 'weather', workspace: { slug: 'vendor' }, categories: ['weather'], mainCategory: 'data', metaTags: ['forecast'], reviewStatus: 'APPROVED', pricingPlans: [{ currentProductConfig: { name: 'Free', pricingPlanConfig: { apiCallLimit: '500', maxTPS: 10, subscriptionPricePerMonth: 0 } } }] };
function html(slug = 'data', products = [product], extras = {}) {
  return `<script id="__NEXT_DATA__" type="application/json">${JSON.stringify({ page: '/category/[category]', query: { category: slug }, isFallback: false, buildId: 'build', props: { pageProps: { publicProducts: products, categoryName: slug, filterTags: [{ value: 'weather', label: 'Weather' }], ...extras } } })}</script>`;
}

test('selects only canonical category pages and validates requested categories before fetching', () => {
  const parsed = parseSitemap(sitemap.replace('</urlset>', '<url><loc>https://api.market/blog/category/finance</loc></url><url><loc>https://evil.example/category/data</loc></url></urlset>'));
  assert.deepEqual(parsed.categories, [url('data'), url('finance')]);
  assert.deepEqual(selectCategories(parsed.categories, ['finance', 'finance']), [url('finance')]);
  assert.throws(() => selectCategories(parsed.categories, ['../api/private']), /Unknown category/);
  assert.throws(() => selectCategories(parsed.categories, ['unknown']), /Unknown category/);
  assert.throws(() => parseSitemap('<urlset/>'), /Unexpected category count/);
});

test('preserves category and plan metadata without inventing a provider website or executable quote', () => {
  const page = parseCategoryPage(html(), url('data'));
  assert.equal(page.count, 1);
  assert.deepEqual(page.subcategories, [{ value: 'weather', label: 'Weather' }]);
  assert.deepEqual(page.products[0].pricing_plans, product.pricingPlans);
  assert.equal(page.products[0].url, 'https://api.market/store/vendor/weather');
  assert.equal(page.products[0].provider_website, null);
  assert.equal(page.products[0].executable, false);
  assert.equal(page.products[0].description, 'Current conditions');
});

test('rejects login/fallback/wrong-category pages, malformed products, and new pagination instead of returning partial success', () => {
  assert.throws(() => parseCategoryPage('<p>Log in</p>', url('data')), /no structured/);
  assert.throws(() => parseCategoryPage(html().replace('"isFallback":false', '"isFallback":true'), url('data')), /Unexpected category page/);
  assert.throws(() => parseCategoryPage(html('finance'), url('data')), /Unexpected category page/);
  assert.throws(() => parseCategoryPage(html('data', [{ ...product, workspace: null }]), url('data')), /Unexpected product/);
  assert.throws(() => parseCategoryPage(html('data', [{ ...product, slug: '../weather' }]), url('data')), /Unexpected product identity/);
  assert.throws(() => parseCategoryPage(html('data', [product], { nextCursor: 'next' }), url('data')), /pagination needs review/);
  assert.throws(() => parseCategoryPage(html('data', [product], { total: 500 }), url('data')), /pagination needs review/);
});

test('deduplicates products across categories while retaining every discovery category and source URL', () => {
  const catalog = buildCatalog({ pages: [parseCategoryPage(html(), url('data')), parseCategoryPage(html('finance'), url('finance'))], requestedUrls: [url('data'), url('finance')], sitemapProducts: ['https://api.market/store/vendor/weather', 'https://api.market/store/vendor/unclassified'] });
  assert.equal(catalog.products.length, 1);
  assert.deepEqual(catalog.products[0].discovered_in, ['data', 'finance']);
  assert.deepEqual(catalog.products[0].source_urls, [url('data'), url('finance')]);
  assert.equal(catalog.report.complete, true);
  assert.equal(catalog.report.category_memberships, 2);
  assert.deepEqual(catalog.report.duplicate_memberships, [{ id: 'vendor/weather', category: 'finance' }]);
  assert.deepEqual(catalog.report.sitemap_products_not_in_selected_categories, ['https://api.market/store/vendor/unclassified']);
});

test('fetches public metadata sequentially with the crawl delay and reports failed categories as incomplete', async () => {
  const events = [];
  const result = await fetchCatalog({ wait: async milliseconds => events.push(['wait', milliseconds]), fetchImpl: async (requestUrl, options) => {
    events.push(['fetch', requestUrl]);
    assert.equal(options.method, 'GET'); assert.equal(options.redirect, 'error');
    assert.equal(options.headers.authorization, undefined);
    if (requestUrl.endsWith('/sitemap.xml')) return new Response(sitemap);
    if (requestUrl === url('data')) return new Response(html());
    return new Response('Temporary error', { status: 503 });
  } });
  assert.deepEqual(events, [['fetch', 'https://api.market/sitemap.xml'], ['wait', 1100], ['fetch', url('data')], ['wait', 1100], ['fetch', url('finance')]]);
  assert.equal(result.report.complete, false);
  assert.equal(result.products.length, 1);
  assert.deepEqual(result.report.failures, [{ url: url('finance'), error: 'HTTP 503' }]);
});

test('category selection limits requests and oversized responses cannot be treated as complete', async () => {
  const requests = [];
  const result = await fetchCatalog({ categories: ['finance'], wait: async () => {}, fetchImpl: async requestUrl => {
    requests.push(requestUrl);
    return new Response(requestUrl.endsWith('sitemap.xml') ? sitemap : 'x'.repeat(5 * 1024 * 1024 + 1));
  } });
  assert.deepEqual(requests, ['https://api.market/sitemap.xml', url('finance')]);
  assert.equal(result.report.complete, false);
  assert.match(result.report.failures[0].error, /exceeds 5 MiB/);
});

test('CLI accepts repeated categories and paths with spaces, and has no apply or paid-execution flags', () => {
  assert.deepEqual(parseArgs(['--category=data', '--category', 'finance', '--out=/tmp/catalog with spaces']).categories, ['data', 'finance']);
  assert.equal(parseArgs(['--out=/tmp/catalog with spaces']).out, '/tmp/catalog with spaces');
  assert.throws(() => parseArgs(['--apply']), /Unknown argument/);
  assert.throws(() => parseArgs(['--category']), /Missing value/);
});
