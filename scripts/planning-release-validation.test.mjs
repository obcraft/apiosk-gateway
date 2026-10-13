import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { assessPreview, matchesNeed, validateCases } from './planning-release-validation.mjs'

const question = 'Find the temperature and rainfall for Example City.'
const needs = [
  { label: 'Temperature', concepts: [['weather', 'temperature']], outputs: [['temperature', 'temp']] },
  { label: 'Rainfall', concepts: [['weather', 'rainfall']], outputs: [['rainfall', 'precipitation']] },
]
const item = { id: 'weather', question, expected_status: 'ready', minimum_sources: 2, expect_needs: needs }
const expiry = '2100-01-01T00:00:00Z'
function response(change = () => {}) {
  const task = {
    conversation: [{ question }],
    capability_intake: {
      catalog_version: 'catalog-version',
      parsed_request: { subjects: [{ id: 's1', label: 'Example City' }], capabilities: needs.map((need, index) => ({
        slug: `weather.${index}`, name: need.label, domain: 'weather', description: 'Weather observations',
        outputs: [need.outputs[0][0]], subject_id: 's1',
      })) },
      matches: needs.map((_, index) => ({ slug: `weather.${index}`, operations: [`observation.${index}`] })),
    },
    intent: { subjects: [{ label: 'Example City', entity_ref: 'entity' }],
      goals: needs.map((_, index) => ({ capability: `observation.${index}`, subject: 0 })) },
    proposal: { independent_calls: true, cap_atomic: 30,
      steps: needs.map((_, index) => ({ goal_index: index, entity_ref: 'entity', service: { id: `service-${index}` } })) },
  }
  change(task)
  return { status: 'ready', total_atomic: '30', expires_at: expiry,
    sources: [10, 20].map((price, index) => ({ source_slug: `source-${index}`, endpoint_id: `endpoint-${index}`, price_atomic: String(price) })),
    preview_token: `${Buffer.from(JSON.stringify({ total_atomic: 30, expires_at: expiry, task })).toString('base64url')}.${'a'.repeat(64)}` }
}

test('complete independent quote preserves every declared category and sums the calls', () => {
  assert.equal(assessPreview(item, response()).accepted, true)
})
test('ready status, a token and a price cannot conceal a lost result category', () => {
  const result = assessPreview(item, response(task => {
    task.capability_intake.parsed_request.capabilities[1].outputs = ['temperature']
  }))
  assert(result.issues.includes('requested_category_lost:Rainfall'))
})
test('description alone cannot claim a missing requested output', () => {
  assert.equal(matchesNeed({ name: 'Weather rainfall', outputs: ['temperature'] }, needs[1]), false)
})
test('two matched categories do not pass if the actual plan drops one', () => {
  const result = assessPreview(item, response(task => { task.proposal.steps[1].goal_index = 0 }))
  assert(result.issues.includes('requested_category_not_in_plan:Rainfall'))
})
test('two operations at one provider do not satisfy the two-source requirement', () => {
  const body = response(); body.sources[1].source_slug = body.sources[0].source_slug
  assert(assessPreview(item, body).issues.includes('too_few_distinct_sources'))
})
test('price mismatches, chained plans, excess calls and altered questions fail', () => {
  let body = response(); body.total_atomic = '29'
  assert(assessPreview(item, body).issues.includes('total_does_not_cover_exactly_the_shown_calls'))
  body = response(task => { task.proposal.independent_calls = false })
  assert(assessPreview(item, body).issues.includes('plan_is_not_independent'))
  body = response(); body.sources = Array(11).fill(body.sources[0])
  assert(assessPreview(item, body).issues.includes('call_limit_exceeded_or_empty'))
  body = response(task => { task.conversation[0].question = 'A different request' })
  assert(assessPreview(item, body).issues.includes('question_changed'))
})
test('a comparison must plan the requested category for each requested subject', () => {
  const comparison = { ...item, expect_needs: [{ ...needs[0], subjects: ['Example City', 'Another City'] }] }
  assert(assessPreview(comparison, response()).issues.includes('requested_category_not_in_plan:Temperature'))
})
test('non-ready cases require the declared status, no quote, and a specific clarification', () => {
  const missing = { ...item, expected_status: 'needs_input', missing_input_terms: [['postal code', 'postcode']] }
  assert.equal(assessPreview(missing, { status: 'needs_input', message: 'What is the postcode?' }).accepted, true)
  assert.equal(assessPreview(missing, { status: 'needs_input', message: 'Choose another service.' }).accepted, false)
  assert.equal(assessPreview(missing, { status: 'unsupported', message: 'What is the postcode?' }).accepted, false)
  assert.equal(assessPreview(missing, { status: 'needs_input', message: 'What is the postcode?', total_atomic: '0' }).accepted, false)
})
test('empty case sets and ready cases without independent expectations cannot pass', () => {
  assert.throws(() => validateCases([]))
  assert.throws(() => validateCases([{ ...item, expect_needs: [] }]))
  assert.throws(() => validateCases([item, item]))
  assert.equal(validateCases([item]).length, 1)
})

test('named-source screening terminology preserves both independent result categories', () => {
  const cases = JSON.parse(readFileSync(new URL('../tests/fixtures/planning-release-cases.json', import.meta.url), 'utf8'))
  const named = cases.find(item => item.id === 'company-search-named-sources')
  assert.equal(named.minimum_sources, 2)
  assert.equal(named.expect_needs.length, 2)
  const screening = { slug: 'compliance.screen.opensanctions', name: 'OpenSanctions screening',
    description: 'Screen the given company name against OpenSanctions data', outputs: ['screening_result'], source_hint: 'opensanctions' }
  const companies = { slug: 'company.search', name: 'Company search',
    description: 'Find companies matching the given name', outputs: ['company_records'], source_hint: 'kvk' }
  assert.equal(matchesNeed(companies, named.expect_needs[0]), true)
  assert.equal(matchesNeed(screening, named.expect_needs[1]), true)
  assert.equal(matchesNeed(companies, named.expect_needs[1]), false, 'company results cannot replace screening')
  assert.equal(matchesNeed(screening, named.expect_needs[0]), false, 'screening cannot replace registry search')
})
