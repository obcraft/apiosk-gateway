//! The reusable intent vocabulary, independent of source selection and execution.
//! Existing definitions come from the taxonomy, including those without offers.
//! New definitions are saved only in the explicitly configured local draft file.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};

const MAX_DRAFT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DRAFTS: usize = 4_096;
const SOURCE_SHORTLIST: usize = 32;
const CORE_DETAILS: usize = 24;
static DRAFT_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Definition {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub input_contract: Value,
    pub output_contract: Value,
    pub status: String,
    #[serde(skip)]
    pub local: bool,
}

/// Read only the taxonomy columns granted to the runtime role. Names/categories
/// are display labels derived from canonical slugs; contracts are not needed for
/// intent classification and are never inferred from the description.
pub async fn load(pool: &PgPool) -> Result<Vec<Definition>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT slug,description,status \
         FROM catalog.capabilities WHERE status IN ('active','draft') ORDER BY slug",
    )
    .fetch_all(pool)
    .await
    .context("intent_capabilities_unavailable")?;
    let definitions = rows
        .into_iter()
        .map(|(slug, description, status)| database_definition(slug, description, status))
        .collect();
    let drafts = match configured_path()? {
        Some(path) => read_drafts(&path).await?,
        None => Vec::new(),
    };
    Ok(merge(definitions, drafts))
}

fn database_definition(slug: String, description: String, status: String) -> Definition {
    let name = slug.replace(['.', '_', '-'], " ");
    let category = if slug.starts_with("operation.") {
        "source_document".into()
    } else {
        slug.split('.').next().unwrap_or_default().to_owned()
    };
    Definition {
        slug,
        name,
        description,
        category,
        input_contract: json!({}),
        output_contract: json!({}),
        status,
        local: false,
    }
}

fn merge(definitions: Vec<Definition>, drafts: Vec<Definition>) -> Vec<Definition> {
    let mut by_slug: BTreeMap<String, Definition> = drafts
        .into_iter()
        .map(|definition| (definition.slug.clone(), definition))
        .collect();
    // Database definitions always win, even over an older local proposal.
    by_slug.extend(
        definitions
            .into_iter()
            .map(|definition| (definition.slug.clone(), definition)),
    );
    by_slug.into_values().collect()
}

fn words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .filter(|word| {
            !matches!(
                *word,
                "the"
                    | "and"
                    | "for"
                    | "with"
                    | "that"
                    | "this"
                    | "from"
                    | "are"
                    | "what"
                    | "how"
                    | "can"
                    | "please"
                    | "find"
                    | "show"
                    | "get"
                    | "look"
                    | "het"
                    | "een"
                    | "van"
                    | "voor"
                    | "met"
                    | "wat"
                    | "hoe"
                    | "zijn"
                    | "dat"
                    | "deze"
                    | "toon"
                    | "zoek"
            )
        })
        .map(str::to_owned)
        .collect()
}

fn score(definition: &Definition, question: &BTreeSet<String>) -> usize {
    [
        (18, &definition.slug),
        (8, &definition.name),
        (1, &definition.description),
    ]
    .into_iter()
    .map(|(weight, text)| weight * words(text).intersection(question).count())
    .sum()
}

fn clipped(text: &str, length: usize) -> String {
    text.chars().take(length).collect()
}

/// Keep the general taxonomy visible for multilingual classification. Only the
/// large source-document section is shortlisted; new drafts are always visible
/// so the next question can reuse them without another definition being made.
pub fn context(definitions: &[Definition], question: &str) -> String {
    let question_words = words(question);
    let definitions: BTreeMap<_, _> = definitions
        .iter()
        .filter(|definition| matches!(definition.status.as_str(), "active" | "draft"))
        .map(|definition| (definition.slug.as_str(), definition))
        .collect();
    let shortlist = |source_document: bool, maximum: usize| {
        let mut ranked: Vec<_> = definitions
            .values()
            .copied()
            .filter(|definition| {
                !definition.local && (definition.category == "source_document") == source_document
            })
            .map(|definition| (score(definition, &question_words), definition.slug.as_str()))
            .filter(|(score, _)| *score > 0)
            .collect();
        ranked.sort_by(|(score_a, slug_a), (score_b, slug_b)| {
            score_b.cmp(score_a).then(slug_a.cmp(slug_b))
        });
        ranked
            .into_iter()
            .take(maximum)
            .map(|(_, slug)| slug)
            .collect::<BTreeSet<_>>()
    };
    let sources = shortlist(true, SOURCE_SHORTLIST);
    let core_details = shortlist(false, CORE_DETAILS);
    let mut catalog: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut details = Vec::new();
    for definition in definitions.into_values() {
        let is_source = definition.category == "source_document";
        if is_source && !definition.local && !sources.contains(definition.slug.as_str()) {
            continue;
        }
        // Preserve everything after the first dot, including nested paths,
        // underscores and hyphens. The prompt reconstructs prefix + '.' + suffix.
        let (prefix, suffix) = definition
            .slug
            .split_once('.')
            .unwrap_or(("", &definition.slug));
        catalog.entry(prefix).or_default().push(suffix);
        if definition.local || is_source || core_details.contains(definition.slug.as_str()) {
            let length = if definition.local || is_source {
                240
            } else {
                160
            };
            let mut detail = json!({"slug":definition.slug,"description":clipped(&definition.description, length)});
            if definition.local {
                detail["name"] = json!(definition.name);
                detail["category"] = json!(definition.category);
                detail["input_contract"] = definition.input_contract.clone();
                detail["output_contract"] = definition.output_contract.clone();
            }
            details.push(detail);
        }
    }
    json!({"catalog":catalog,"details":details}).to_string()
}

fn configured_path() -> Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os("ASK_INTENT_DRAFT_PATH") else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    ensure!(
        path.is_absolute() && path.file_name().is_some(),
        "intent_draft_path_must_be_absolute"
    );
    Ok(Some(path))
}

fn valid_slug(slug: &str) -> bool {
    let mut parts = slug.split('.');
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    };
    let Some(first) = parts.next() else {
        return false;
    };
    let remaining: Vec<_> = parts.collect();
    slug.len() <= 160
        && valid(first)
        && !remaining.is_empty()
        && remaining.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        })
}

fn valid_draft(definition: &Definition) -> bool {
    valid_slug(&definition.slug)
        && !definition.name.trim().is_empty()
        && definition.name.chars().count() <= 240
        && !definition.description.trim().is_empty()
        && definition.description.chars().count() <= 2_000
        && !definition.category.trim().is_empty()
        && definition.category.chars().count() <= 100
        && definition.input_contract.is_object()
        && definition.output_contract.is_object()
}

async fn read_drafts(path: &Path) -> Result<Vec<Definition>> {
    let file = match fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("intent_drafts_unavailable"),
    };
    let mut bytes = Vec::new();
    file.take(MAX_DRAFT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .context("intent_drafts_unavailable")?;
    ensure!(bytes.len() <= MAX_DRAFT_BYTES, "intent_drafts_too_large");
    let mut drafts: Vec<Definition> =
        serde_json::from_slice(&bytes).context("intent_drafts_invalid_json")?;
    let mut slugs = BTreeSet::new();
    ensure!(
        drafts.len() <= MAX_DRAFTS
            && drafts.iter().all(|definition| {
                definition.status == "draft"
                    && valid_draft(definition)
                    && slugs.insert(definition.slug.as_str())
            }),
        "intent_drafts_invalid_definition"
    );
    for draft in &mut drafts {
        draft.local = true;
    }
    Ok(drafts)
}

/// Save reusable vocabulary locally; this never creates database capabilities,
/// source offers or executable contracts. An existing slug stays canonical.
pub async fn save_drafts(definitions: &[Definition]) -> Result<Vec<Definition>> {
    if definitions.is_empty() {
        return Ok(Vec::new());
    }
    let path = configured_path()?.context("intent_draft_storage_not_configured")?;
    save_drafts_at(&path, definitions).await
}

async fn save_drafts_at(path: &Path, definitions: &[Definition]) -> Result<Vec<Definition>> {
    let _lock = DRAFT_LOCK.lock().await;
    ensure!(
        definitions.iter().all(valid_draft),
        "intent_drafts_invalid_definition"
    );
    let mut drafts: BTreeMap<_, _> = read_drafts(path)
        .await?
        .into_iter()
        .map(|definition| (definition.slug.clone(), definition))
        .collect();
    for definition in definitions {
        drafts.entry(definition.slug.clone()).or_insert_with(|| {
            let mut draft = definition.clone();
            draft.status = "draft".into();
            draft.local = true;
            draft
        });
    }
    ensure!(drafts.len() <= MAX_DRAFTS, "intent_drafts_too_large");
    let canonical = definitions
        .iter()
        .map(|definition| drafts[&definition.slug].clone())
        .collect();
    let bytes = serde_json::to_vec_pretty(&drafts.into_values().collect::<Vec<_>>())?;
    ensure!(bytes.len() <= MAX_DRAFT_BYTES, "intent_drafts_too_large");
    let parent = path.parent().context("intent_draft_path_invalid")?;
    fs::create_dir_all(parent)
        .await
        .context("intent_drafts_unavailable")?;
    let temporary = parent.join(format!(".ask-intent-drafts-{}.tmp", uuid::Uuid::new_v4()));
    let result = async {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, path).await
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result.context("intent_drafts_unavailable")?;
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(slug: &str, category: &str) -> Definition {
        Definition {
            slug: slug.into(),
            name: format!("Capability {slug}"),
            description: format!("Retrieve reusable {slug} information"),
            category: category.into(),
            input_contract: json!({"type":"object"}),
            output_contract: json!({"type":"object"}),
            status: "active".into(),
            local: false,
        }
    }

    #[test]
    fn database_definition_wins_over_local_draft() {
        let canonical = definition("company.profile", "company");
        let mut old_draft = canonical.clone();
        old_draft.status = "draft".into();
        old_draft.description = "Obsolete local meaning".into();
        let local = definition("general.conversation", "general");
        assert_eq!(
            merge(vec![canonical.clone()], vec![old_draft, local.clone()]),
            vec![canonical, local]
        );
    }

    #[test]
    fn database_display_labels_are_derived_without_inventing_contracts() {
        let definition = database_definition(
            "company_registry.lookup".into(),
            "Canonical description".into(),
            "active".into(),
        );
        assert_eq!(definition.name, "company registry lookup");
        assert_eq!(definition.category, "company_registry");
        assert_eq!(definition.description, "Canonical description");
        assert_eq!(definition.input_contract, json!({}));
        assert_eq!(definition.output_contract, json!({}));
        assert!(!definition.local);
        let operation = database_definition(
            "operation.provider.source-lookup".into(),
            "Source definition".into(),
            "draft".into(),
        );
        assert_eq!(operation.category, "source_document");
        assert_eq!(operation.name, "operation provider source lookup");
        assert_eq!(operation.status, "draft");
    }

    #[test]
    fn compact_database_context_preserves_relevant_meaning_without_duplicate_labels() {
        let relevant = database_definition(
            "company_registry.lookup".into(),
            "Canonical company description".into(),
            "active".into(),
        );
        let unrelated = database_definition(
            "general.conversation".into(),
            "A friendly conversation".into(),
            "active".into(),
        );
        let output: Value =
            serde_json::from_str(&context(&[relevant, unrelated], "Company details")).unwrap();
        assert_eq!(output["catalog"]["company_registry"], json!(["lookup"]));
        assert_eq!(output["catalog"]["general"], json!(["conversation"]));
        let rows = output["details"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows.iter().all(|row| row.get("name").is_none()));
        assert!(rows.iter().all(|row| row.get("category").is_none()));
        assert_eq!(rows[0]["description"], "Canonical company description");
    }

    #[test]
    fn source_shortlist_is_bounded_without_hiding_taxonomy_or_drafts() {
        let mut definitions = (0..45)
            .map(|index| definition(&format!("source.glassdoor-{index:02}"), "source_document"))
            .collect::<Vec<_>>();
        let core = definition("company_registry.lookup", "company_registry");
        definitions.push(core.clone());
        definitions.push(definition("source.unrelated", "source_document"));
        let mut draft = definition("source.new-request", "source_document");
        draft.status = "draft".into();
        draft.local = true;
        definitions.push(draft.clone());
        let output: Value =
            serde_json::from_str(&context(&definitions, "Compare Glassdoor ratings")).unwrap();
        let rows = output["details"].as_array().unwrap();
        assert_eq!(rows.len(), SOURCE_SHORTLIST + 1);
        assert_eq!(output["catalog"]["company_registry"], json!(["lookup"]));
        assert_eq!(
            output["catalog"]["source"].as_array().unwrap().len(),
            SOURCE_SHORTLIST + 1
        );
        assert!(rows.iter().any(|row| row["slug"] == draft.slug));
        assert!(!rows.iter().any(|row| row["slug"] == "source.unrelated"));
        let draft_detail = rows.iter().find(|row| row["slug"] == draft.slug).unwrap();
        assert_eq!(draft_detail["name"], draft.name);
        assert_eq!(draft_detail["input_contract"], draft.input_contract);
        assert_eq!(draft_detail["output_contract"], draft.output_contract);
        assert_eq!(
            context(&definitions, "Compare Glassdoor ratings"),
            context(
                &definitions.into_iter().rev().collect::<Vec<_>>(),
                "Compare Glassdoor ratings"
            )
        );
    }

    #[test]
    fn distinctive_name_beats_common_description_and_unicode_clipping_is_safe() {
        let query = words("Glassdoor employer ratings");
        let mut named = definition("source.named", "source_document");
        named.name = "Glassdoor".into();
        named.description = "界".repeat(300);
        let mut described = definition("source.other", "source_document");
        described.description = "employer ratings".into();
        assert!(score(&named, &query) > score(&described, &query));
        let output: Value = serde_json::from_str(&context(&[named], "Glassdoor")).unwrap();
        assert_eq!(
            output["details"][0]["description"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            240
        );
    }

    #[test]
    fn grouping_preserves_every_core_slug_with_bounded_details() {
        let definitions: Vec<_> = (0..40)
            .map(|index| {
                definition(
                    &format!("company_registry.nested.profile-{index:02}"),
                    "company_registry",
                )
            })
            .collect();
        let output: Value =
            serde_json::from_str(&context(&definitions, "company profile")).unwrap();
        let reconstructed: BTreeSet<_> = output["catalog"]
            .as_object()
            .unwrap()
            .iter()
            .flat_map(|(prefix, suffixes)| {
                suffixes
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(move |suffix| format!("{prefix}.{}", suffix.as_str().unwrap()))
            })
            .collect();
        assert_eq!(
            reconstructed,
            definitions
                .iter()
                .map(|definition| definition.slug.clone())
                .collect()
        );
        assert_eq!(output["details"].as_array().unwrap().len(), CORE_DETAILS);
        assert!(output["details"]
            .as_array()
            .unwrap()
            .iter()
            .all(|detail| detail.as_object().unwrap().len() == 2));
    }

    #[test]
    fn slugs_preserve_current_database_grammar() {
        for slug in [
            "company_registry.lookup",
            "web.page-to-text",
            "general.conversation",
            "company.some_field",
        ] {
            assert!(valid_slug(slug), "{slug}");
        }
        for slug in [
            "company",
            ".lookup",
            "company.",
            "company..lookup",
            "Company.lookup",
            "company-registry.lookup",
            "../escape",
        ] {
            assert!(!valid_slug(slug), "{slug}");
        }
    }

    #[tokio::test]
    async fn draft_writes_are_atomic_reusable_and_preserve_existing_meaning() {
        let directory = std::env::temp_dir().join(format!(
            "apiosk-intent-drafts-test-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("drafts.json");
        let first = definition("general.conversation", "general");
        let second = definition("research.unfamiliar-topic", "research");
        let first_batch = [first.clone()];
        let second_batch = [second];
        let (one, two) = tokio::join!(
            save_drafts_at(&path, &first_batch),
            save_drafts_at(&path, &second_batch)
        );
        assert!(one.unwrap()[0].local);
        assert!(two.unwrap()[0].local);
        let saved = read_drafts(&path).await.unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved
            .iter()
            .all(|definition| definition.status == "draft" && definition.local));
        let mut redefinition = first.clone();
        redefinition.description = "Changed meaning must not replace existing definition".into();
        let authoritative = save_drafts_at(&path, &[redefinition]).await.unwrap();
        assert_eq!(authoritative[0].description, first.description);
        assert!(authoritative[0].local);
        assert!(serde_json::to_value(&authoritative[0])
            .unwrap()
            .get("local")
            .is_none());
        let reloaded = read_drafts(&path).await.unwrap();
        assert_eq!(
            reloaded
                .iter()
                .find(|definition| definition.slug == first.slug)
                .unwrap()
                .description,
            first.description
        );
        assert_eq!(
            fs::read_dir(&directory)
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .unwrap()
                .file_name(),
            "drafts.json"
        );
        fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_drafts_do_not_replace_the_saved_file() {
        let directory = std::env::temp_dir().join(format!(
            "apiosk-intent-drafts-test-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("drafts.json");
        let valid = definition("general.conversation", "general");
        save_drafts_at(&path, std::slice::from_ref(&valid)).await.unwrap();
        let before = fs::read(&path).await.unwrap();
        let mut invalid = valid;
        invalid.input_contract = json!([]);
        assert!(save_drafts_at(&path, &[invalid]).await.is_err());
        assert_eq!(fs::read(&path).await.unwrap(), before);
        fs::write(&path, "{broken").await.unwrap();
        assert!(
            save_drafts_at(&path, &[definition("general.other", "general")])
                .await
                .is_err()
        );
        assert_eq!(fs::read(&path).await.unwrap(), b"{broken");
        fs::remove_dir_all(directory).await.unwrap();
    }
}
