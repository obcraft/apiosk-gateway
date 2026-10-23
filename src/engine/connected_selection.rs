//! A connected buyer's explicit source basket. Preparing or repricing it can
//! never authorize, dispatch, or charge a source.
use super::*;
use crate::{ask_v2::{source_options::SourceOption, source_selection::SelectedSource}, capability_intake::{Intake, ParsedRequest}};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectedChoice {
    pub endpoint_id: Uuid,
    pub capability: String,
    #[serde(default)]
    pub subject_id: Option<String>,
    #[serde(default)]
    pub input: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectedPrepareRequest {
    pub request_id: Uuid,
    pub question: String,
    pub parsed_request: ParsedRequest,
    pub search_id: String,
    pub selections: Vec<ConnectedChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedSelection {
    pub question: String,
    pub original_intake: Intake,
    pub search_id: String,
    pub selections: Vec<SelectedSource>,
    pub(crate) options: Vec<SourceOption>,
}
impl ConnectedSelection {
    pub fn view(&self) -> Value {
        json!({"question":self.question,"parsed_request":self.original_intake.parsed_request,
            "original_intake":self.original_intake,"search_id":self.search_id,"selections":self.selections,
            "selection":{"version":1,"mode":"include","sources":self.selections},"options":self.options,
            "can_choose":true,"max_sources":MAX_STEPS})
    }
}

fn choices(request: &ConnectedPrepareRequest, options: &[SourceOption]) -> anyhow::Result<Vec<SelectedSource>> {
    anyhow::ensure!(!request.selections.is_empty() && request.selections.len() <= MAX_STEPS, SOURCE_LIMIT_MESSAGE);
    let mut seen = BTreeSet::new();
    request.selections.iter().map(|choice| {
        anyhow::ensure!(!choice.endpoint_id.is_nil() && choice.input.len() <= 16, "Choose a valid source and its declared inputs");
        let offered: Vec<_> = options.iter().filter(|option|
            option.source.endpoint_id.as_deref() == Some(choice.endpoint_id.to_string().as_str())
                && option.source.capability.as_deref() == Some(choice.capability.as_str())
                && choice.subject_id.as_ref().is_none_or(|id| &option.subject_id == id)).collect();
        anyhow::ensure!(offered.len() == 1, "Select an offered source and its exact subject");
        let subject_id = offered[0].subject_id.clone();
        anyhow::ensure!(seen.insert((choice.endpoint_id, choice.capability.clone(), subject_id.clone())), "A source was selected twice for the same subject");
        Ok(SelectedSource {endpoint_id:choice.endpoint_id,capability:choice.capability.clone(),subject_id,input:choice.input.clone()})
    }).collect()
}

impl Engine {
    pub(crate) async fn prepare_connected_sources(&self, p: &Principal, req: ConnectedPrepareRequest,
        snapshot: crate::search_snapshot::Snapshot) -> anyhow::Result<Response> {
        anyhow::ensure!(p.connection.is_some() && !req.request_id.is_nil(), "Source selection requires a connected caller");
        if let Some(response) = self.request_claim(p, req.request_id,
            &digest(&("connected-source-prepare",p.connection,&req)), "discover").await? { return Ok(response); }
        let result = self.prepare_connected_sources_inner(p, &req, snapshot).await;
        Ok(self.complete(p, req.request_id, result).await)
    }

    async fn prepare_connected_sources_inner(&self, p: &Principal, req: &ConnectedPrepareRequest,
        snapshot: crate::search_snapshot::Snapshot) -> anyhow::Result<Response> {
        let catalog = self.catalog.get().await?;
        if !snapshot.current(&catalog.version) {
            return Ok(Response::error(req.request_id, Status::NeedsInput, "source_refresh_required",
                "The source search expired or changed. Search again before reviewing the selected sources. Nothing was purchased."));
        }
        let options = snapshot.source_options().to_vec();
        let selections = choices(req, &options)?;
        let original = snapshot.intake(req.parsed_request.clone());
        let mut task = Task {workspace_id:p.workspace,..Default::default()};
        task.connected_lookup_required=crate::parser::connected_planning::needs_native_lookup(&catalog,&selections);
        let prepared = crate::parser::connected_planning::compile_selected(&req.question, &task, &catalog, &original, &selections, &options)?;
        task.connected_selection = Some(ConnectedSelection {question:req.question.clone(),original_intake:original,
            search_id:req.search_id.clone(),selections,options});
        task.capability_intake = Some(prepared.intake);
        task.connected_coverage = Some(prepared.coverage);
        task.record_turn(&req.question, (!prepared.reply.discover).then_some(prepared.reply.message.clone()));
        task.output_grounding = task.capability_intake.as_ref().and_then(|intake|
            crate::parser::source_grounding::Grounding::new(intake, &catalog, prepared.reply.output_bindings));
        let mut intent = prepared.reply.intent.ok_or_else(||anyhow::anyhow!("The selected sources need more input before a quote can be prepared"))?;
        input::bind_subjects(&mut task, &catalog, &mut intent)?;
        name_lookup::seed(&mut task, &catalog, &mut intent)?;
        task.intent = Some(serde_json::to_value(&intent)?);
        let status = self.plan(&mut task, &catalog, &intent)?;
        if let Some(value) = &task.proposal {
            let plan: Plan = serde_json::from_value(value.clone())?;
            let mut required = BTreeMap::new();
            for selected in &task.connected_selection.as_ref().unwrap().selections {
                *required.entry((selected.endpoint_id,selected.capability.as_str())).or_insert(0usize) += 1;
            }
            for ((endpoint,capability),count) in required {
                anyhow::ensure!(plan.steps.iter().filter(|step| step.service.endpoint_id == endpoint
                    && step.service.contract.capability_slug == capability).count() >= count,
                    "The quoted plan must include every explicitly selected source");
            }
        }
        anyhow::ensure!(task.bounded(), "Task capacity reached");
        let id = self.store.create_scoped(p.owner,p.connection,p.workspace).await?;
        self.store.link_request_task(p.owner,req.request_id,id).await?;
        let (revision,_,_) = self.store.load_scoped(p,id).await?;
        self.store.save_intent(p.owner,id,revision,&task).await?;
        self.view(p,id,req.request_id,Some(status)).await
    }

    pub(crate) async fn requote_connected_sources(&self, p:&Principal, req:RequoteRequest) -> anyhow::Result<Response> {
        anyhow::ensure!(p.connection.is_some() && !req.request_id.is_nil(), "Repricing requires a connected caller");
        self.signer.verify(p.owner,&req.state,true)?;
        let (_,_,connection) = self.store.load_scoped(p,req.state.state_ref).await?;
        anyhow::ensure!(connection == p.connection, "This request belongs to another connection.");
        if let Some(response) = self.request_claim(p,req.request_id,&digest(&("connected-source-requote",p.connection,&req)),"requote").await? {return Ok(response);}
        let result = self.requote_inner(p,req.state.state_ref,&req,true).await;
        Ok(self.complete(p,req.request_id,result).await)
    }
}
