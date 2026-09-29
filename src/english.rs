//! English renderings for Ask search.
//!
//! Ask search matches words, but catalog texts are written in many languages
//! (CBS, KVK and overheid in Dutch, Sirenic in French, ...) and a question may
//! come in any language. Search therefore also reads everything in English,
//! next to the original wording, never instead of it:
//!
//! - Catalog: after each load, the texts a cheap check does not find clearly
//!   English (service names and descriptions, capability descriptions, source
//!   names, descriptions, categories and tags) are translated in the
//!   background, many per call, and kept in memory by a hash of the text. A
//!   refresh translates only texts it has not seen. The search index adds the
//!   English rendering of each text it indexes.
//! - Request: a capability object that is not English is translated within a
//!   short deadline and searched with both wordings.
//! - Bazaar: a listing that is not English is translated the same way before
//!   its topic is checked.
//!
//! The only model is Apiosk's, Qwen3 30B via OpenRouter (`parser::MODEL`).
//! Nothing is written to the database, and logs carry counts and durations,
//! never text. Whatever is unavailable (the `ASK_V2_TRANSLATION` flag off, no
//! key, a timeout, a refusal) leaves a text as it is: search then behaves
//! exactly as it does without translation.
use crate::{
    ask_v2::search::stems,
    capability_intake::{Capability, ParsedRequest},
    catalog::Catalog,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant},
};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

/// Set to `0`, `false`, `off` or `no` to switch translation off; on by default.
pub const FLAG: &str = "ASK_V2_TRANSLATION";
const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const PROMPT: &str = include_str!("../contracts/catalog-translation-prompt.txt");
/// As the Ask parse: Qwen's own provider first, OpenRouter's others after it.
const PROVIDERS: &[&str] = &["alibaba"];
/// A text's first characters are what identifies it; longer ones are clipped
/// for the model (the hash still covers the whole text).
const MAX_INPUT_CHARS: usize = 600;
const MAX_OUTPUT_CHARS: usize = 600;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Folding and the cheap language check
// ---------------------------------------------------------------------------

/// Lowercases `character` without its accents: `é` is `e`, `ß` is `ss`, `ø`
/// is `o`. Letters of other scripts (Cyrillic, Greek, CJK) are kept.
pub fn push_folded(character: char, out: &mut String) {
    if character.is_ascii() {
        out.push(character.to_ascii_lowercase());
        return;
    }
    for lower in character.to_lowercase() {
        match lower {
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ø' => out.push('o'),
            'ł' => out.push('l'),
            'đ' | 'ð' => out.push('d'),
            'þ' => out.push_str("th"),
            'ı' => out.push('i'),
            _ => out.extend(
                std::iter::once(lower)
                    .nfd()
                    .filter(|part| !is_combining_mark(*part)),
            ),
        }
    }
}

/// `text` lowercased and without accents; see [`push_folded`].
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        push_folded(character, &mut out);
    }
    out
}

/// Han, kana and Hangul: scripts written without spaces between words.
pub fn unspaced(character: char) -> bool {
    matches!(character,
        '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}' | '\u{ac00}'..='\u{d7af}' | '\u{20000}'..='\u{2ffff}')
}

fn latin(character: char) -> bool {
    character.is_ascii_alphabetic()
        || matches!(character, '\u{00c0}'..='\u{024f}' | '\u{1e00}'..='\u{1eff}')
}

/// English function words. Words other languages share (`in`, `is`, `a`,
/// `an`, `was`, `per`, `die`) are left out.
const ENGLISH_WORDS: &[&str] = &[
    "the", "and", "of", "to", "for", "with", "from", "by", "are", "or", "not", "as", "at", "this",
    "that", "these", "those", "its", "their", "be", "been", "it", "which", "into", "than", "only",
    "each", "were", "has", "have", "can", "will", "any", "all", "such", "when", "where", "who",
    "what", "how", "you", "your", "our", "they",
];
/// Function words of other European languages, folded. Words English texts
/// also carry as codes or names (`es`, `se`, `un`, `au`, `est`, `mit`, `uber`,
/// `com`, `os`, `do`) are left out.
const OTHER_WORDS: &[&str] = &[
    // Dutch
    "de", "het", "een", "van", "en", "voor", "met", "op", "naar", "niet", "bij", "zijn", "wordt",
    "worden", "uit", "aan", "door", "ook", "tot", "deze", "dit", "om", "te", "welke", "waar",
    "wat", "hoe", "geen", "alle", "haar", "hun", "dan", // French
    "le", "la", "les", "des", "du", "une", "et", "pour", "avec", "sur", "dans", "par", "aux", "ou",
    "pas", "cette", "ces", "qui", "que", "sont", "leur", "leurs", // German
    "der", "die", "das", "und", "fur", "von", "auf", "ist", "nicht", "dem", "den", "ein", "eine",
    "einer", "eines", "zum", "zur", "im", "bei", "nach", "sind", "wird", "werden", "oder", "auch",
    "sowie", // Spanish
    "el", "los", "las", "del", "para", "con", "por", "una", "como", "sus", "y",
    // Italian
    "il", "di", "gli", "della", "delle", "dei", "degli", "sono", "nel", "nella", "alla", "che",
    // Portuguese
    "da", "em", "uma", "nao", "ao", "pelo", "pela", // Scandinavian
    "och", "og", "af", "av", "til", "fra", "att", "som", "ett", "ikke", "eller",
];

/// A function word of another language (`het`, `les`, `und`, `los`): no more
/// a topic than "the" is. Folded, as search compares words.
pub fn function_word(word: &str) -> bool {
    OTHER_WORDS.contains(&word)
}

/// What the cheap check can tell about a text's language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    /// English function words and no others: nothing to translate.
    English,
    /// Another script, or another language's function words.
    Other,
    /// Too short to tell by function words: a name, a tag list, a title.
    Unclear,
}

/// A cheap check, no model: the text's script and function words.
pub fn detect(text: &str) -> Language {
    let (mut letters, mut latin_letters) = (0usize, 0usize);
    for character in text.chars().filter(|c| c.is_alphabetic()) {
        letters += 1;
        if latin(character) {
            latin_letters += 1;
        }
    }
    if letters == 0 {
        // Codes and numbers only: nothing to translate.
        return Language::English;
    }
    if latin_letters * 5 < letters * 4 {
        return Language::Other;
    }
    let folded = fold(text);
    let (mut english, mut other) = (0usize, 0usize);
    for word in folded.split(|c: char| !c.is_alphanumeric()) {
        if ENGLISH_WORDS.contains(&word) {
            english += 1;
        } else if OTHER_WORDS.contains(&word) {
            other += 1;
        }
    }
    match (english, other) {
        (0, 0) => Language::Unclear,
        (_, 0) => Language::English,
        // An English text naming a Dutch term ("jaarrekening, winst- en
        // verliesrekening") stays English.
        (english, other) if english >= 2 && english >= 3 * other => Language::English,
        _ => Language::Other,
    }
}

/// Words long enough to tell a language apart, as search reads them: no
/// acronyms, codes, numbers or short words.
fn content_words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|raw| {
            raw.chars().count() >= 5
                && raw.chars().all(char::is_alphabetic)
                && raw.chars().any(char::is_lowercase)
        })
        .flat_map(stems)
        .filter(|word| word.chars().count() >= 5)
        .collect()
}

/// Whether a text the function words could not place reads as English: every
/// content word is one the catalog's English texts use.
fn known(text: &str, lexicon: &HashSet<String>) -> bool {
    content_words(text)
        .iter()
        .all(|word| lexicon.contains(word))
}

/// The key under which a text's English rendering is kept.
type Key = [u8; 16];
fn key(text: &str) -> Key {
    let digest = Sha256::digest(text.as_bytes());
    let mut key = [0; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

// ---------------------------------------------------------------------------
// The memory the search index reads
// ---------------------------------------------------------------------------

/// English renderings of catalog texts, by a hash of the original text. One
/// memory outlives catalog refreshes; the search index reads it.
pub struct Memory {
    id: u64,
    renderings: RwLock<HashMap<Key, Arc<str>>>,
    generation: AtomicU64,
    /// Words of the catalog's English texts and renderings, for texts the
    /// function words cannot place (names, tags, short capabilities).
    lexicon: RwLock<Arc<HashSet<String>>>,
}
impl Default for Memory {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            renderings: RwLock::default(),
            generation: AtomicU64::new(0),
            lexicon: RwLock::default(),
        }
    }
}
impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory")
            .field("renderings", &self.len())
            .field("generation", &self.generation())
            .finish()
    }
}
impl Memory {
    /// Identifies this memory for the search index cache.
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Changes whenever renderings are added.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    pub fn len(&self) -> usize {
        self.renderings
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn english(&self, text: &str) -> Option<Arc<str>> {
        let renderings = self.renderings.read().unwrap_or_else(|p| p.into_inner());
        if renderings.is_empty() || text.trim().is_empty() {
            return None;
        }
        renderings.get(&key(text)).cloned()
    }
    /// The English renderings of `texts` that say something the text does
    /// not, joined by spaces; empty when there are none.
    pub fn render(&self, texts: &[&str]) -> String {
        let renderings = self.renderings.read().unwrap_or_else(|p| p.into_inner());
        if renderings.is_empty() {
            return String::new();
        }
        texts
            .iter()
            .filter(|text| !text.trim().is_empty())
            .filter_map(|text| {
                renderings
                    .get(&key(text))
                    .filter(|english| &***english != *text)
                    .cloned()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    /// Keeps renderings; returns how many were new.
    pub(crate) fn remember(&self, texts: &[String], english: &[Option<String>]) -> usize {
        let mut added = 0;
        {
            let mut renderings = self.renderings.write().unwrap_or_else(|p| p.into_inner());
            for (text, rendered) in texts.iter().zip(english) {
                if let Some(rendered) = rendered {
                    if renderings
                        .insert(key(text), Arc::from(rendered.as_str()))
                        .is_none()
                    {
                        added += 1;
                    }
                }
            }
        }
        if added > 0 {
            let mut lexicon = (*self.lexicon()).clone();
            for rendered in english.iter().flatten() {
                lexicon.extend(stems(rendered));
            }
            *self.lexicon.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(lexicon);
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        added
    }
    fn contains(&self, key: &Key) -> bool {
        self.renderings
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(key)
    }
    fn lexicon(&self) -> Arc<HashSet<String>> {
        self.lexicon
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    /// Forgets renderings of texts the catalog no longer has. Search never
    /// asks for them again, so the index is unaffected.
    fn retain(&self, current: &HashSet<Key>) {
        let mut renderings = self.renderings.write().unwrap_or_else(|p| p.into_inner());
        if renderings.len() > current.len() {
            renderings.retain(|key, _| current.contains(key));
        }
    }
}

/// A catalog's tags as one text, for translation and the index alike.
pub fn tags_text(tags: &[String]) -> String {
    tags.join(", ")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Sentences: descriptions. Their function words tell the language.
    Prose,
    /// Names, tags and categories.
    Label,
}

/// Every text the search index reads from `catalog`, once each.
fn units(catalog: &Catalog) -> Vec<(&str, Kind)> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut units: Vec<(&str, Kind)> = Vec::new();
    let mut texts: Vec<(&str, Kind)> = Vec::new();
    for service in &catalog.services {
        texts.push((&service.name, Kind::Label));
        if let Some(description) = &service.description {
            texts.push((description, Kind::Prose));
        }
    }
    for source in &catalog.known_sources {
        texts.push((&source.name, Kind::Label));
        texts.push((&source.metadata.description, Kind::Prose));
        texts.push((&source.metadata.category, Kind::Label));
        if let Some(descriptions) = source.metadata.capability_descriptions.as_object() {
            texts.extend(
                descriptions
                    .values()
                    .filter_map(Value::as_str)
                    .map(|text| (text, Kind::Prose)),
            );
        }
    }
    for (text, kind) in texts {
        if text.trim().is_empty() {
            continue;
        }
        match seen.get(text) {
            Some(&position) if kind == Kind::Prose => units[position].1 = Kind::Prose,
            Some(_) => {}
            None => {
                seen.insert(text, units.len());
                units.push((text, kind));
            }
        }
    }
    units
}

/// Every source's tag text (see [`tags_text`]); owned, unlike [`units`].
fn tag_units(catalog: &Catalog) -> Vec<String> {
    catalog
        .known_sources
        .iter()
        .filter(|source| !source.metadata.tags.is_empty())
        .map(|source| tags_text(&source.metadata.tags))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

// ---------------------------------------------------------------------------
// The translator
// ---------------------------------------------------------------------------

/// Why a translation call gave nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A 4xx other than 408 and 429: the same call would be refused again.
    Refused(u16),
    /// A timeout, transport failure, 408, 429 or 5xx: worth one more try.
    Unavailable,
    /// The reply was not the promised JSON.
    Invalid,
}

pub type Translation<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Option<String>>, Failure>> + Send + 'a>>;

/// English renderings of several texts in one call, in the texts' order;
/// `None` where the reply has none for a text.
pub trait Translator: Send + Sync {
    fn translate<'a>(&'a self, texts: &'a [String]) -> Translation<'a>;
}

/// Apiosk's model through OpenRouter, a JSON object per call.
pub struct Qwen {
    http: reqwest::Client,
    key: String,
    endpoint: String,
    timeout: Duration,
}
impl Qwen {
    pub fn new(http: reqwest::Client, key: String) -> Self {
        Self {
            http,
            key,
            endpoint: ENDPOINT.into(),
            timeout: Limits::default().call_timeout,
        }
    }
    /// Local fixtures only: production has a fixed OpenRouter endpoint.
    #[doc(hidden)]
    pub fn with_local_endpoint(mut self, endpoint: String) -> anyhow::Result<Self> {
        let url = url::Url::parse(&endpoint)?;
        anyhow::ensure!(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                && url.username().is_empty()
                && url.password().is_none(),
            "invalid local model endpoint"
        );
        self.endpoint = endpoint;
        Ok(self)
    }
    fn body(texts: &[String]) -> Value {
        let items: Vec<Value> = texts
            .iter()
            .enumerate()
            .map(|(index, text)| json!({"id": (index + 1).to_string(), "text": clip(text, MAX_INPUT_CHARS)}))
            .collect();
        json!({
            "model": crate::parser::MODEL,
            "messages": [{"role":"system","content":PROMPT},{"role":"user","content":json!({"texts":items}).to_string()}],
            "temperature": 0, "max_tokens": (256 + 96 * texts.len()).min(4096),
            "provider": {"order": PROVIDERS, "require_parameters": true},
            "response_format": {"type": "json_object"}
        })
    }
    async fn call(&self, texts: &[String]) -> Result<Vec<Option<String>>, Failure> {
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.key)
            .header("X-Title", "Apiosk catalog translation")
            .timeout(self.timeout)
            .json(&Self::body(texts))
            .send()
            .await
            .map_err(|_| Failure::Unavailable)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(match status {
                408 | 429 => Failure::Unavailable,
                400..=499 => Failure::Refused(status),
                _ => Failure::Unavailable,
            });
        }
        let (body, _) = crate::parser::bounded_json_sized(response, MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| Failure::Invalid)?;
        let (prompt, completion) = (
            body.pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            body.pointer("/usage/completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        tracing::debug!(
            texts = texts.len(),
            prompt_tokens = prompt,
            completion_tokens = completion,
            "catalog translation call"
        );
        let content = body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or(Failure::Invalid)?;
        decode(content, texts.len())
    }
}
impl Translator for Qwen {
    fn translate<'a>(&'a self, texts: &'a [String]) -> Translation<'a> {
        Box::pin(self.call(texts))
    }
}

/// The model's `{"texts":[{"id","english"}]}`, in the input order. Unknown or
/// repeated ids, empty, overlong or control-character renderings count as
/// missing; a reply with none at all is invalid.
fn decode(content: &str, count: usize) -> Result<Vec<Option<String>>, Failure> {
    let value = crate::capability_intake::extract_json(content).map_err(|_| Failure::Invalid)?;
    let items = value["texts"].as_array().ok_or(Failure::Invalid)?;
    let mut english: Vec<Option<String>> = vec![None; count];
    for item in items {
        let id = match &item["id"] {
            Value::String(id) => id.trim().parse::<usize>().ok(),
            Value::Number(id) => id.as_u64().map(|id| id as usize),
            _ => None,
        };
        let Some(slot) = id
            .and_then(|id| id.checked_sub(1))
            .filter(|slot| *slot < count)
        else {
            continue;
        };
        let Some(text) = item["english"].as_str().map(str::trim) else {
            continue;
        };
        if english[slot].is_none()
            && !text.is_empty()
            && text.chars().count() <= MAX_OUTPUT_CHARS
            && !text.chars().any(char::is_control)
        {
            english[slot] = Some(text.to_owned());
        }
    }
    if count > 0 && english.iter().all(Option::is_none) {
        return Err(Failure::Invalid);
    }
    Ok(english)
}

// ---------------------------------------------------------------------------
// Translation for search
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Limits {
    /// Texts per call, and their characters.
    pub batch_items: usize,
    pub batch_chars: usize,
    /// Calls in flight at once during a catalog pass.
    pub concurrency: usize,
    pub call_timeout: Duration,
    /// Waits before the retries of a call that was unavailable (not refused).
    pub retry_delays: Vec<Duration>,
    /// The whole translation of a request or of Bazaar listings, which a
    /// person is waiting for; one attempt.
    pub request_deadline: Duration,
    /// How long a text that failed waits before a later pass tries it again.
    pub failure_backoff: Duration,
    pub refusal_backoff: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            batch_items: 24,
            batch_chars: 9_000,
            concurrency: 3,
            call_timeout: Duration::from_secs(45),
            retry_delays: vec![Duration::from_secs(2), Duration::from_secs(6)],
            request_deadline: Duration::from_secs(3),
            failure_backoff: Duration::from_secs(600),
            refusal_backoff: Duration::from_secs(3600),
        }
    }
}

/// Counts of one catalog pass, logged without any text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub pending: usize,
    pub translated: usize,
    pub failed: usize,
    pub calls: usize,
    pub ms: u64,
}

/// What a catalog load leaves to translate.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub units: usize,
    /// Clearly English already.
    pub english: usize,
    /// Translated by an earlier pass.
    pub cached: usize,
    /// Failed recently; retried after their backoff.
    pub backing_off: usize,
    pub pending: Vec<String>,
}

#[derive(Default)]
struct PassState {
    running: bool,
    /// Texts that failed, until when they are left alone.
    failed: HashMap<Key, Instant>,
}

/// A request's capability objects for search: `original` as the person's
/// parse has them, `english` the same with every non-English name,
/// description, input and output in English. Search reads both wordings.
/// Without a translation `english` is `original`.
#[derive(Clone, Debug)]
pub struct Normalized {
    pub original: ParsedRequest,
    pub english: ParsedRequest,
}

type Rendered = Arc<Vec<Option<String>>>;

pub struct English {
    memory: Arc<Memory>,
    translator: Option<Arc<dyn Translator>>,
    limits: Limits,
    state: Mutex<PassState>,
    /// Request and Bazaar translations, by a hash of the texts sent.
    requests: moka::future::Cache<Key, Rendered>,
}

impl English {
    /// No translation: search reads every text as it is.
    pub fn disabled() -> Self {
        Self::build(None, Limits::default())
    }
    pub fn new(translator: Arc<dyn Translator>, limits: Limits) -> Self {
        Self::build(Some(translator), limits)
    }
    fn build(translator: Option<Arc<dyn Translator>>, limits: Limits) -> Self {
        Self {
            memory: Arc::default(),
            translator,
            limits,
            state: Mutex::default(),
            requests: moka::future::Cache::builder()
                .max_capacity(4096)
                .time_to_live(Duration::from_secs(24 * 3600))
                .build(),
        }
    }
    /// On unless `ASK_V2_TRANSLATION` switches it off, and only with a key.
    pub fn from_env(http: reqwest::Client, key: &str) -> Self {
        if !enabled(std::env::var(FLAG).ok().as_deref()) {
            tracing::info!(flag = FLAG, "Ask search translation is off");
            return Self::disabled();
        }
        if key.trim().is_empty() {
            tracing::warn!(
                "Ask search translation needs an OpenRouter key; searching original texts only"
            );
            return Self::disabled();
        }
        Self::new(Arc::new(Qwen::new(http, key.to_owned())), Limits::default())
    }
    pub fn enabled(&self) -> bool {
        self.translator.is_some()
    }
    pub fn memory(&self) -> Arc<Memory> {
        self.memory.clone()
    }
    fn state(&self) -> std::sync::MutexGuard<'_, PassState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Gives a freshly loaded catalog this memory and starts translating its
    /// new texts in the background, unless a pass is already running (the
    /// next load picks up what it leaves).
    pub fn attach(self: &Arc<Self>, catalog: &mut Catalog) {
        catalog.english = self.memory.clone();
        if self.translator.is_none() {
            return;
        }
        let plan = self.plan(catalog);
        if plan.pending.is_empty() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        {
            let mut state = self.state();
            if state.running {
                return;
            }
            state.running = true;
        }
        tracing::info!(
            units = plan.units,
            english = plan.english,
            cached = plan.cached,
            backing_off = plan.backing_off,
            pending = plan.pending.len(),
            "Ask search catalog translation started"
        );
        let this = self.clone();
        runtime.spawn(async move {
            struct Running(Arc<English>);
            impl Drop for Running {
                fn drop(&mut self) {
                    self.0.state().running = false;
                }
            }
            let running = Running(this);
            let report = running.0.translate(plan.pending).await;
            tracing::info!(
                pending = report.pending,
                translated = report.translated,
                failed = report.failed,
                calls = report.calls,
                ms = report.ms,
                "Ask search catalog translation finished"
            );
        });
    }

    /// The catalog's texts that are not clearly English and have no English
    /// rendering yet. Also refreshes the English lexicon and forgets
    /// renderings of texts the catalog no longer has.
    pub fn plan(&self, catalog: &Catalog) -> Plan {
        let mut texts: Vec<(String, Kind)> = units(catalog)
            .into_iter()
            .map(|(text, kind)| (text.to_owned(), kind))
            .collect();
        texts.extend(
            tag_units(catalog)
                .into_iter()
                .map(|text| (text, Kind::Label)),
        );
        let languages: Vec<Language> = texts.iter().map(|(text, _)| detect(text)).collect();
        let mut lexicon: HashSet<String> = ENGLISH_WORDS
            .iter()
            .map(|word| (*word).to_owned())
            .collect();
        for ((text, kind), language) in texts.iter().zip(&languages) {
            if *kind == Kind::Prose && *language == Language::English {
                lexicon.extend(stems(text));
            }
        }
        for (text, _) in &texts {
            if let Some(english) = self.memory.english(text) {
                lexicon.extend(stems(&english));
            }
        }
        *self
            .memory
            .lexicon
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Arc::new(lexicon);
        let lexicon = self.memory.lexicon();
        let current: HashSet<Key> = texts.iter().map(|(text, _)| key(text)).collect();
        self.memory.retain(&current);
        let now = Instant::now();
        let mut state = self.state();
        state
            .failed
            .retain(|key, until| current.contains(key) && now < *until);
        let mut plan = Plan {
            units: texts.len(),
            ..Plan::default()
        };
        for ((text, _), language) in texts.into_iter().zip(languages) {
            let english = match language {
                Language::English => true,
                Language::Other => false,
                Language::Unclear => known(&text, &lexicon),
            };
            let key = key(&text);
            if english {
                plan.english += 1;
            } else if self.memory.contains(&key) {
                plan.cached += 1;
            } else if state.failed.contains_key(&key) {
                plan.backing_off += 1;
            } else {
                plan.pending.push(text);
            }
        }
        plan
    }

    /// Translates `pending` catalog texts, several per call and a few calls
    /// at a time, into the memory. A refusal is not retried, and a refused
    /// key (401, 403) ends the pass.
    pub async fn translate(&self, pending: Vec<String>) -> Report {
        let started = Instant::now();
        let mut report = Report {
            pending: pending.len(),
            ..Report::default()
        };
        let Some(translator) = self.translator.clone() else {
            return report;
        };
        let mut batches: Vec<Vec<String>> = Vec::new();
        let mut size = 0;
        for text in pending {
            let length = text.chars().count().min(MAX_INPUT_CHARS);
            let full = batches.last().is_none_or(|batch| {
                batch.len() >= self.limits.batch_items || size + length > self.limits.batch_chars
            });
            if full {
                batches.push(Vec::new());
                size = 0;
            }
            size += length;
            batches.last_mut().expect("a batch").push(text);
        }
        let stop = AtomicBool::new(false);
        let mut results = stream::iter(batches)
            .map(|batch| {
                let (translator, stop) = (translator.clone(), &stop);
                async move {
                    if stop.load(Ordering::Acquire) {
                        return (batch, Err(Failure::Refused(0)), 0);
                    }
                    let (result, calls) = self.attempt(translator.as_ref(), &batch).await;
                    if matches!(result, Err(Failure::Refused(401 | 403))) {
                        stop.store(true, Ordering::Release);
                    }
                    (batch, result, calls)
                }
            })
            .buffer_unordered(self.limits.concurrency.max(1));
        while let Some((batch, result, calls)) = results.next().await {
            report.calls += calls;
            let now = Instant::now();
            match result {
                Ok(english) => {
                    report.translated += self.memory.remember(&batch, &english);
                    let mut state = self.state();
                    for (text, rendered) in batch.iter().zip(&english) {
                        if rendered.is_none() {
                            report.failed += 1;
                            state
                                .failed
                                .insert(key(text), now + self.limits.failure_backoff);
                        }
                    }
                }
                Err(failure) => {
                    report.failed += batch.len();
                    let backoff = match failure {
                        Failure::Refused(_) => self.limits.refusal_backoff,
                        _ => self.limits.failure_backoff,
                    };
                    let mut state = self.state();
                    for text in &batch {
                        state.failed.insert(key(text), now + backoff);
                    }
                }
            }
        }
        report.ms = started.elapsed().as_millis() as u64;
        report
    }

    /// One batch: a call, and up to two more after a delay when the model was
    /// unavailable. Returns the result and the calls made.
    async fn attempt(
        &self,
        translator: &dyn Translator,
        texts: &[String],
    ) -> (Result<Vec<Option<String>>, Failure>, usize) {
        let mut calls = 0;
        let mut delays = self.limits.retry_delays.iter();
        loop {
            calls += 1;
            let result =
                tokio::time::timeout(self.limits.call_timeout, translator.translate(texts))
                    .await
                    .unwrap_or(Err(Failure::Unavailable))
                    .and_then(|english| {
                        if english.len() == texts.len() {
                            Ok(english)
                        } else {
                            Err(Failure::Invalid)
                        }
                    });
            match (&result, delays.next()) {
                (Err(Failure::Unavailable), Some(delay)) => tokio::time::sleep(*delay).await,
                _ => return (result, calls),
            }
        }
    }

    /// One bounded translation for a person who is waiting: cached by the
    /// texts, a single attempt within the request deadline. A late answer
    /// still lands in the cache, for the next search of the same wording.
    async fn rendered(&self, texts: Vec<String>) -> Option<Rendered> {
        let translator = self.translator.clone()?;
        if texts.is_empty() {
            return None;
        }
        let key = key(&texts.join("\u{1f}"));
        let started = Instant::now();
        let requests = self.requests.clone();
        let work = tokio::spawn(async move {
            requests
                .try_get_with(key, async move {
                    match translator.translate(&texts).await {
                        Ok(english) if english.len() == texts.len() => Ok(Arc::new(english)),
                        Ok(_) => Err(Failure::Invalid),
                        Err(failure) => Err(failure),
                    }
                })
                .await
        });
        match tokio::time::timeout(self.limits.request_deadline, work).await {
            Ok(Ok(Ok(english))) => Some(english),
            outcome => {
                let reason = match outcome {
                    Err(_) => "timeout",
                    Ok(Err(_)) => "unavailable",
                    Ok(Ok(Err(failure))) => match *failure {
                        Failure::Refused(_) => "refused",
                        Failure::Unavailable => "unavailable",
                        Failure::Invalid => "invalid_output",
                    },
                    Ok(Ok(Ok(_))) => "ok",
                };
                tracing::warn!(
                    reason,
                    ms = started.elapsed().as_millis() as u64,
                    "Ask search translation unavailable; searching the original wording"
                );
                None
            }
        }
    }

    /// The capability objects in English for search. A capability whose
    /// name, description, inputs and outputs are not clearly English is
    /// translated. A request in English, or any failure, leaves `english`
    /// equal to the request.
    pub async fn request(&self, parsed: &ParsedRequest) -> Normalized {
        let unchanged = || Normalized {
            original: parsed.clone(),
            english: parsed.clone(),
        };
        if self.translator.is_none() || parsed.language == "en" {
            return unchanged();
        }
        let lexicon = self.memory.lexicon();
        let foreign: Vec<usize> = parsed
            .capabilities
            .iter()
            .enumerate()
            .filter(|(_, capability)| {
                capability.slug != "general.conversation" && !reads_english(capability, &lexicon)
            })
            .map(|(position, _)| position)
            .collect();
        if foreign.is_empty() {
            return unchanged();
        }
        // Name, description, inputs and outputs of each, in that order.
        let fields: Vec<String> = foreign
            .iter()
            .flat_map(|position| {
                let capability = &parsed.capabilities[*position];
                [
                    capability.name.clone(),
                    capability.description.clone(),
                    concepts_text(&capability.inputs),
                    concepts_text(&capability.outputs),
                ]
            })
            .collect();
        let sent: Vec<String> = fields
            .iter()
            .filter(|text| !text.is_empty())
            .cloned()
            .collect();
        let started = Instant::now();
        let Some(rendered) = self.rendered(sent).await else {
            return unchanged();
        };
        tracing::info!(
            capabilities = foreign.len(),
            ms = started.elapsed().as_millis() as u64,
            "Ask search read capabilities in English"
        );
        let mut answers = rendered.iter();
        let english: Vec<Option<String>> = fields
            .iter()
            .map(|text| {
                if text.is_empty() {
                    None
                } else {
                    answers.next().cloned().flatten()
                }
            })
            .collect();
        let mut normalized = unchanged();
        for (position, english) in foreign.iter().zip(english.chunks(4)) {
            normalized.english.capabilities[*position] =
                in_english(&parsed.capabilities[*position], english);
        }
        normalized
    }

    /// English renderings of Bazaar listing texts that are not English,
    /// within the request deadline; empty when there are none or on failure.
    pub async fn listings(&self, texts: &[String]) -> HashMap<String, Arc<str>> {
        let foreign: Vec<String> = texts
            .iter()
            .filter(|text| detect(text) == Language::Other)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if foreign.is_empty() || self.translator.is_none() {
            return HashMap::new();
        }
        let Some(rendered) = self.rendered(foreign.clone()).await else {
            return HashMap::new();
        };
        foreign
            .into_iter()
            .zip(rendered.iter())
            .filter_map(|(text, english)| {
                english.as_deref().map(|english| (text, Arc::from(english)))
            })
            .collect()
    }
}

/// `ASK_V2_TRANSLATION`: on unless it says `0`, `false`, `off` or `no`.
fn enabled(value: Option<&str>) -> bool {
    !value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

fn concepts_text(concepts: &[String]) -> String {
    concepts
        .iter()
        .map(|concept| concept.replace('_', " "))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether a capability object already reads as English.
fn reads_english(capability: &Capability, lexicon: &HashSet<String>) -> bool {
    let text = format!(
        "{}. {}. {}. {}",
        capability.name,
        capability.description,
        concepts_text(&capability.inputs),
        concepts_text(&capability.outputs)
    );
    match detect(&text) {
        Language::English => true,
        Language::Other => false,
        // Without a lexicon yet, nothing marks the text as foreign.
        Language::Unclear => lexicon.is_empty() || known(&text, lexicon),
    }
}

fn snake(value: &str) -> String {
    value
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn concepts(text: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for concept in text.split([',', ';', '\n']).map(snake) {
        if !concept.is_empty()
            && concept.len() <= 100
            && !result.contains(&concept)
            && result.len() < 32
        {
            result.push(concept);
        }
    }
    result
}

/// `original` with the renderings of its name, description, inputs and
/// outputs, where there are any.
fn in_english(original: &Capability, english: &[Option<String>]) -> Capability {
    let field = |position: usize| english.get(position).cloned().flatten();
    let mut capability = original.clone();
    if let Some(name) = field(0) {
        capability.name = clip(&name, 120);
    }
    if let Some(description) = field(1) {
        capability.description = clip(&description, 1000);
    }
    if let Some(inputs) = field(2)
        .map(|inputs| concepts(&inputs))
        .filter(|inputs| !inputs.is_empty())
    {
        capability.inputs = inputs;
    }
    if let Some(outputs) = field(3)
        .map(|outputs| concepts(&outputs))
        .filter(|outputs| !outputs.is_empty())
    {
        capability.outputs = outputs;
    }
    capability
}

#[cfg(test)]
pub(crate) mod tests;
