//! Shape constraints complement, never replace, saved-source validation.
use serde_json::{json, Value};
fn object(properties: Value) -> Value {
    let required = properties
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub(super) fn format(max_observations: usize, max_limitations: usize) -> Value {
    let prose = json!({"type":"string","pattern":"^[^0-9]*$","minLength":1,"maxLength":2000});
    let reference = object(json!({"fact_id":{"type":"string","pattern":"^f[1-9][0-9]*$"}}));
    let label = object(json!({"label":{"type":"string","minLength":1,"maxLength":160}}));
    let heading = json!({"anyOf":[reference.clone(),label.clone()]});
    let cell = json!({"anyOf":[reference.clone(),label,{"type":"null"}]});
    let presentation = json!({"anyOf":[object(json!({"kind":{"type":"string","enum":["text","document"]}})),object(json!({
        "kind":{"type":"string","enum":["table","comparison","chart"]},"title":{"type":"string","minLength":1,"maxLength":160},
        "columns":{"type":"array","items":heading,"minItems":2,"maxItems":8},
        "rows":{"type":"array","items":{"type":"array","items":cell,"minItems":2,"maxItems":8},"maxItems":50}
    }))]});
    let schema = object(json!({
        "observations":{"type":"array","maxItems":max_observations,"items":object(json!({"text":prose.clone(),"evidence":{"type":"array","items":reference,"minItems":1,"maxItems":8}}))},
        "limitations":{"type":"array","maxItems":max_limitations,"items":prose},
        "follow_up_question":{"type":["string","null"]},"presentation":presentation,
        "requirement_status":{"type":"array","items":object(json!({"requirement_id":{"type":"string"},"status":{"type":"string","enum":["answered","unknown","not_applicable"]},"answered_by":{"type":"array","items":{"type":"string","pattern":"^o[1-9][0-9]*$"}},"reason":{"type":["string","null"],"enum":[null,"not_in_source","source_failed","source_pending","not_fetched","ambiguous_identity","entity_mismatch","period_unavailable","outside_coverage"]}}))},
        "source_gaps":{"type":"array","items":object(json!({"result_ref":{"type":"string"},"reason":{"type":"string","enum":["not_in_source","entity_mismatch","period_unavailable","outside_coverage"]},"limitation_index":{"type":"integer","minimum":0}}))}
    }));
    json!({"type":"json_schema","json_schema":{"name":"saved_source_analysis","strict":true,"schema":schema}})
}
