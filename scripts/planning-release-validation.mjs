// Acceptance assertions only. These expectations never route production
// questions, select providers, or supply answers to the planner.
import assert from 'node:assert/strict'

const words = text => String(text).toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean)
const contains = (text, term) => {
  const actual = words(text), wanted = words(term)
  return wanted.length > 0 && actual.some((_, index) => wanted.every((word, offset) =>
    actual[index + offset] === word || actual[index + offset]?.replace(/s$/, '') === word))
}
const groupsMatch = (text, groups) => groups.every(group => group.some(term => contains(text, term)))

export function matchesNeed(capability, need) {
  const outputs = capability.outputs ?? []
  const meaning = [capability.slug, capability.name, capability.description, capability.domain, ...outputs].join(' ')
  const narrowed = need.insufficient_output_terms?.length > 0 && outputs.every(output =>
    need.insufficient_output_terms.some(term => contains(output, term)))
  return outputs.length > 0 && !narrowed && groupsMatch(meaning, need.concepts)
    && groupsMatch(outputs.join(' '), need.outputs ?? [])
}

export function validateCases(cases) {
  assert(Array.isArray(cases) && cases.length > 0, 'At least one acceptance case is required')
  assert.equal(new Set(cases.map(item => item.id)).size, cases.length, 'Case IDs must be unique')
  for (const item of cases) {
    assert(typeof item.id === 'string' && item.id.length > 0, 'Each case needs an ID')
    assert(typeof item.question === 'string' && item.question.trim().length > 0, `${item.id}: missing question`)
    assert(['ready', 'needs_input', 'unsupported'].includes(item.expected_status), `${item.id}: declare the expected outcome`)
    assert(Number.isInteger(item.minimum_sources) && item.minimum_sources >= 1 && item.minimum_sources <= 10,
      `${item.id}: minimum_sources must be between 1 and 10`)
    if (item.expected_status === 'ready') assert(Array.isArray(item.expect_needs) && item.expect_needs.length > 0,
      `${item.id}: declare the required result categories independently of the model`)
    for (const need of item.expect_needs ?? []) {
      assert(typeof need.label === 'string' && need.label.length > 0, `${item.id}: name every required category`)
      assert(Array.isArray(need.concepts) && need.concepts.length > 0, `${item.id}: category concepts required`)
      if (need.subjects) assert(Array.isArray(need.subjects) && need.subjects.length > 0
        && need.subjects.every(subject => typeof subject === 'string' && words(subject).length > 0), `${item.id}: invalid expected subjects`)
      for (const group of [...need.concepts, ...(need.outputs ?? [])]) {
        assert(Array.isArray(group) && group.length > 0 && group.every(term => typeof term === 'string' && words(term).length > 0),
          `${item.id}: semantic groups need nonempty alternatives`)
      }
    }
    if (item.expected_status === 'needs_input') {
      assert(Array.isArray(item.missing_input_terms) && item.missing_input_terms.length > 0,
        `${item.id}: needs_input must identify the actual missing information`)
      assert(item.missing_input_terms.every(group => Array.isArray(group) && group.length > 0
        && group.every(term => typeof term === 'string' && words(term).length > 0)), `${item.id}: invalid missing input groups`)
    }
  }
  return cases
}

function unsignedPreview(token) {
  // This inspects the response from the configured HTTPS service. It is not a
  // signature verifier and must never be used to authorize or execute a token.
  if (typeof token !== 'string' || token.length > 250_000 || !/^[\w-]+\.[a-f0-9]{64}$/.test(token)) return null
  try { return JSON.parse(Buffer.from(token.split('.')[0], 'base64url').toString('utf8')) } catch { return null }
}
function atomic(value) {
  return (typeof value === 'string' && /^\d+$/.test(value)) || (Number.isSafeInteger(value) && value >= 0)
    ? BigInt(value) : null
}

export function assessPreview(item, body, { httpOk = true, now = Date.now() } = {}) {
  const issues = []
  const check = (condition, issue) => { if (!condition) issues.push(issue) }
  check(httpOk, 'http_error')
  check(body?.status === item.expected_status, 'unexpected_status')
  if (item.expected_status !== 'ready') {
    check(body?.preview_token == null && body?.total_atomic == null, 'unready_request_exposes_quote')
    check(typeof body?.message === 'string' && body.message.trim().length > 0, 'missing_actionable_message')
    if (item.expected_status === 'needs_input') {
      check(groupsMatch(body?.message ?? '', item.missing_input_terms), 'clarification_does_not_name_missing_information')
    }
    return { accepted: issues.length === 0, issues }
  }

  const sources = Array.isArray(body?.sources) ? body.sources : []
  const providers = new Set(sources.map(source => source.source_slug).filter(Boolean))
  check(providers.size >= item.minimum_sources, 'too_few_distinct_sources')
  check(sources.length > 0 && sources.length <= 10, 'call_limit_exceeded_or_empty')
  check(sources.every(source => typeof source.endpoint_id === 'string' && source.endpoint_id.length > 0), 'missing_endpoint_identity')
  const total = atomic(body?.total_atomic)
  const prices = sources.map(source => atomic(source.price_atomic))
  check(total !== null && prices.every(price => price !== null), 'invalid_price')
  if (total !== null && prices.every(price => price !== null)) {
    check(prices.reduce((sum, price) => sum + price, 0n) === total, 'total_does_not_cover_exactly_the_shown_calls')
  }
  const preview = unsignedPreview(body?.preview_token)
  check(preview !== null, 'missing_or_malformed_preview')
  if (!preview) return { accepted: false, issues }
  const task = preview.task
  const intake = task?.capability_intake
  const parsed = intake?.parsed_request
  const plan = task?.proposal
  check(atomic(preview.total_atomic) === total && atomic(plan?.cap_atomic) === total, 'preview_price_mismatch')
  check(Date.parse(preview.expires_at) > now && preview.expires_at === body.expires_at, 'expired_or_mismatched_preview')
  check(task?.conversation?.some(turn => turn.question === item.question), 'question_changed')
  check(typeof intake?.catalog_version === 'string' && intake.catalog_version.length > 0, 'missing_catalog_version')
  check(plan?.independent_calls === true && !plan.workflow_slug, 'plan_is_not_independent')
  check(plan?.steps?.length === sources.length, 'displayed_and_quoted_call_counts_differ')
  check(plan?.steps?.every(step => step.browser_fallback == null), 'conditional_source_call')
  const capabilities = parsed?.capabilities ?? []
  const goals = task?.intent?.goals ?? []
  const subjects = task?.intent?.subjects ?? []
  const steps = plan?.steps ?? []
  const categories = (item.expect_needs ?? []).map(need => {
    const matched = capabilities.filter(capability => matchesNeed(capability, need))
    const hasPlan = (capability, expectedSubject) => {
      const found = intake?.matches?.find(match => match.slug === capability.slug)
      const parsedSubject = parsed.subjects?.find(subject => subject.id === capability.subject_id)
      return goals.some((goal, index) => found?.operations?.includes(goal.capability)
        && (!parsedSubject || subjects[goal.subject]?.label === parsedSubject.label)
        && (!expectedSubject || words(subjects[goal.subject]?.label ?? '').join(' ') === words(expectedSubject).join(' '))
        && steps.some(step => step.goal_index === index && step.entity_ref === subjects[goal.subject]?.entity_ref))
    }
    const planned = need.subjects?.length ? need.subjects.every(label => matched.some(capability => hasPlan(capability, label)))
      : matched.some(capability => hasPlan(capability))
    check(matched.length > 0, `requested_category_lost:${need.label}`)
    check(planned, `requested_category_not_in_plan:${need.label}`)
    return { category: need.label, retained: matched.length > 0, planned }
  })
  for (const input of item.literal_inputs ?? []) {
    check((task?.facts ?? []).some(fact => fact.value === input), `supplied_input_not_bound:${input}`)
  }
  return { accepted: issues.length === 0, issues, categories, catalog_version: intake?.catalog_version,
    distinct_sources: providers.size, calls: sources.length }
}
