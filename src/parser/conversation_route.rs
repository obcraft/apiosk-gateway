//! Interpret a turn before source discovery. This decision never authorizes
//! execution; saved answers can only select server-owned conversation outputs.
use super::*;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Route {
    SavedAnswer { outputs: Vec<usize> },
    NewRequest,
    ContinueRequest,
    Clarification { message: String },
}

fn validate_route(raw: &str, outputs: &Value) -> anyhow::Result<Route> {
    let route: Route = serde_json::from_str(raw)?;
    match &route {
        Route::SavedAnswer { outputs: selected } => anyhow::ensure!(
            !selected.is_empty() && selected.len() <= 6
            && selected.iter().all(|index| outputs.as_array().is_some_and(|items| *index < items.len()))
            && selected.iter().collect::<std::collections::BTreeSet<_>>().len() == selected.len(),
            "invalid saved output selection"),
        Route::Clarification { message } => { valid_follow_up(message)?; },
        _ => {},
    }
    Ok(route)
}

impl Parser {
    /// One short routing request before a follow-up inside a saved task: can
    /// the saved results answer it, is it a continuation, a new question or
    /// genuinely unclear? Bounded JSON on the small instruct model; the
    /// question itself is then interpreted by the one Ask parse as usual.
    pub(crate) async fn conversation_route(&self, question: &str, task: &Task, outputs: &Value) -> anyhow::Result<Route> {
        let payload = json!({"model":self.model,"temperature":0,"max_tokens":700,
            "response_format":{"type":"json_object"},
            "messages":[{"role":"system","content":include_str!("../../contracts/conversation-route-prompt.txt")},
                {"role":"user","content":json!({"question":question,"conversation":task.parser_context(),"saved_outputs":outputs}).to_string()}]});
        self.completion("conversation-route", &payload, |raw| validate_route(raw,outputs)).await
    }
}

/// A new topic keeps the durable chat, but its source search must not inherit
/// a prior company, source, quote, requirement or assistant clarification.
pub(crate) fn interpretation_context(task: &Task, new_topic: bool) -> Task {
    if new_topic { Task { capability_intake: task.capability_intake.clone(), ..Task::default() } }
    else { task.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_route_only_accepts_distinct_existing_outputs_and_no_actions() {
        let outputs=json!([{"question":"A"},{"question":"B"}]);
        assert!(validate_route(r#"{"kind":"saved_answer","outputs":[0,1]}"#,&outputs).is_ok());
        for raw in [r#"{"kind":"saved_answer","outputs":[]}"#,r#"{"kind":"saved_answer","outputs":[2]}"#,
            r#"{"kind":"saved_answer","outputs":[0,0]}"#,r#"{"kind":"saved_answer","outputs":[0],"url":"https://other.test"}"#,
            r#"{"kind":"execute","outputs":[0]}"#,r#"{"kind":"clarification","message":""}"#] {
            assert!(validate_route(raw,&outputs).is_err(),"{raw}");
        }
    }
    #[test]
    fn independent_question_does_not_inherit_old_entities_or_requirements() {
        let mut task=Task::default();task.record_turn("Old company",Some("What year?".into()));
        task.intent=Some(json!({"goals":[{"source":"old-source"}]}));
        task.entities.push(crate::state::Entity{entity_ref:Uuid::new_v4(),label:"Old company".into(),selected:true});
        let independent=interpretation_context(&task,true);
        assert!(independent.conversation.is_empty() && independent.entities.is_empty() && independent.intent.is_none());
        assert_eq!(interpretation_context(&task,false).conversation.len(),1);
        assert_eq!(task.conversation.len(),1,"durable history is retained");
    }
}
