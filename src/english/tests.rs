use super::*;
use crate::{
    capability_intake::normalize,
    catalog::{KnownSource, Service, Snapshot},
    readiness::Metadata,
};
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

/// A translator without a network: renderings from a dictionary, calls
/// counted, and scripted failures before the dictionary answers.
#[derive(Default)]
pub(crate) struct Fake {
    dictionary: HashMap<String, String>,
    pub calls: AtomicUsize,
    pub batches: Mutex<Vec<usize>>,
    failures: Mutex<VecDeque<Failure>>,
    delay: Option<Duration>,
}
impl Fake {
    pub(crate) fn new(dictionary: &[(&str, &str)]) -> Self {
        Self {
            dictionary: dictionary
                .iter()
                .map(|(text, english)| ((*text).to_owned(), (*english).to_owned()))
                .collect(),
            ..Self::default()
        }
    }
    fn failing(self, failures: &[Failure]) -> Self {
        *self.failures.lock().unwrap() = failures.iter().copied().collect();
        self
    }
    fn slow(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
impl Translator for Fake {
    fn translate<'a>(&'a self, texts: &'a [String]) -> Translation<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.batches.lock().unwrap().push(texts.len());
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(failure) = self.failures.lock().unwrap().pop_front() {
                return Err(failure);
            }
            Ok(texts
                .iter()
                .map(|text| self.dictionary.get(text).cloned())
                .collect())
        })
    }
}

/// Limits for tests: small batches, no waits between retries.
pub(crate) fn limits() -> Limits {
    Limits {
        batch_items: 3,
        batch_chars: 9_000,
        concurrency: 1,
        call_timeout: Duration::from_secs(5),
        retry_delays: vec![Duration::ZERO, Duration::ZERO],
        request_deadline: Duration::from_millis(500),
        failure_backoff: Duration::from_secs(600),
        refusal_backoff: Duration::from_secs(3600),
    }
}

/// A runtime for a synchronous test to wait on translation with.
pub(crate) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// Gives `catalog` the English renderings a background pass would leave,
/// from `dictionary`, without a runtime of the caller's own. The returned
/// translator also serves requests, as `CatalogCache`'s does.
pub(crate) fn translate_catalog(
    catalog: &mut Catalog,
    dictionary: &[(&str, &str)],
) -> (Arc<English>, Plan, Report) {
    let english = Arc::new(English::new(
        Arc::new(Fake::new(dictionary)),
        Limits {
            batch_items: 24,
            ..limits()
        },
    ));
    catalog.english = english.memory();
    let plan = english.plan(catalog);
    let report = runtime().block_on(english.translate(plan.pending.clone()));
    (english, plan, report)
}

fn service(id: u128, capability: &str, name: &str, description: &str, source: &str) -> Service {
    let fixture: Snapshot = serde_json::from_str(include_str!(
        "../../tests/fixtures/integration-catalog.json"
    ))
    .unwrap();
    let mut service = fixture.services[0].clone();
    service.id = uuid::Uuid::from_u128(id);
    service.endpoint_id = uuid::Uuid::from_u128(id + 1000);
    service.api_slug = source.into();
    service.contract.capability_slug = capability.into();
    service.name = name.into();
    service.description = Some(description.into());
    service
}
fn source(slug: &str, name: &str, description: &str, tags: &[&str]) -> KnownSource {
    KnownSource {
        slug: slug.into(),
        name: name.into(),
        block_reason: None,
        metadata: Metadata {
            description: description.into(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            ..Default::default()
        },
    }
}
fn catalog(services: Vec<Service>, known_sources: Vec<KnownSource>) -> Catalog {
    let fixture: Snapshot = serde_json::from_str(include_str!(
        "../../tests/fixtures/integration-catalog.json"
    ))
    .unwrap();
    Catalog::build(Snapshot {
        services,
        known_sources,
        vocabulary: fixture.vocabulary,
    })
}

const CBS_TITLE: &str = "Werkloosheid; kerncijfers per maand, seizoengecorrigeerd";
const CBS_TITLE_EN: &str = "Unemployment; key figures per month, seasonally adjusted";
const INSEE: &str = "Taux de chômage au sens du BIT, par trimestre et par région";
const INSEE_EN: &str = "ILO unemployment rate, by quarter and by region";
const TAGS: &str = "werkloosheid, beroepsbevolking, arbeidsmarkt";
const TAGS_EN: &str = "unemployment, labour force, labour market";

fn mixed() -> Catalog {
    catalog(
        vec![
            service(
                1,
                "operation.cbs.get-werkloosheid",
                "CBS Statistics",
                CBS_TITLE,
                "cbs-statline",
            ),
            service(2, "operation.insee.get-chomage", "INSEE", INSEE, "insee"),
            service(
                3,
                "company.officers",
                "Companies House",
                "Retrieve the current directors and officers of a company from Companies House.",
                "companies-house",
            ),
            service(
                4,
                "news.latest",
                "Reuters News",
                "Latest Reuters headlines with the link and the publication time.",
                "reuters",
            ),
        ],
        vec![
            source(
                "cbs-statline",
                "CBS Statistics",
                "Official Dutch statistics for the labour market and the economy.",
                &["werkloosheid", "beroepsbevolking", "arbeidsmarkt"],
            ),
            source(
                "reuters",
                "Reuters News",
                "Latest news headlines from Reuters, with links.",
                &["news", "headlines"],
            ),
        ],
    )
}
fn dictionary() -> Vec<(&'static str, &'static str)> {
    vec![
        (CBS_TITLE, CBS_TITLE_EN),
        (INSEE, INSEE_EN),
        (TAGS, TAGS_EN),
    ]
}

#[test]
fn folding_drops_accents_and_keeps_other_scripts() {
    for (text, folded) in [
        ("Économie Française", "economie francaise"),
        ("Straße", "strasse"),
        ("Brønnøysundregistrene", "bronnoysundregistrene"),
        ("Ærø Łódź Œuvre", "aero lodz oeuvre"),
        ("Tasa de paro en España", "tasa de paro en espana"),
        ("Москва", "москва"),
        ("東京の人口", "東京の人口"),
        ("ΦΠΑ", "φπα"),
    ] {
        assert_eq!(fold(text), folded, "{text}");
    }
}

#[test]
fn the_cheap_check_tells_english_from_other_languages_without_a_model() {
    for text in [
        "Retrieve the official registration record for a company from a national business register.",
        "Latest Reuters headlines with the link and the publication time.",
        // English naming Dutch terms stays English.
        "Retrieve structured financial data from the annual accounts a company filed with its business register: balance sheet, income statement, cash flow statement, consolidated variants, financial year and filing date. Official filed figures (jaarrekening, balans, winst- en verliesrekening, deponering), not estimates or modelled revenue.",
        "Direct RDW technical profile by plate, excluding fuel/emissions",
        "2024-01, 83149",
    ] {
        assert_eq!(detect(text), Language::English, "{text}");
    }
    for text in [
        "Faillissementen; bedrijven en instellingen, SBI 2008",
        "Taux de chômage au sens du BIT, par trimestre et par région",
        "Arbeitslosenquote nach Bundesländern und Geschlecht",
        "Tasa de paro por comunidad autónoma y sexo",
        "Indice dei prezzi al consumo per l'intera collettività",
        "Безработица по регионам",
        "都道府県別の失業率",
    ] {
        assert_eq!(detect(text), Language::Other, "{text}");
    }
    for text in [
        "Werkloosheid; kerncijfers",
        "CBS Statistics",
        "faillissementen, bedrijfstak, branche",
        "81156NED",
    ] {
        assert_eq!(detect(text), Language::Unclear, "{text}");
    }
    // Unclear texts are placed by the words the catalog's English uses; a
    // code has no such word and is left alone.
    let lexicon: HashSet<String> =
        stems("Official statistics of the labour market and company bankruptcies")
            .into_iter()
            .collect();
    assert!(known("81156NED", &lexicon));
    assert!(known("Bankruptcies; statistics", &lexicon));
    assert!(!known("Werkloosheid; kerncijfers", &lexicon));
}

#[test]
fn a_pass_translates_only_texts_that_are_not_english_and_remembers_them() {
    let mut catalog = mixed();
    let fake = Arc::new(Fake::new(&dictionary()));
    let english = English::new(fake.clone(), limits());
    catalog.english = english.memory();
    let plan = english.plan(&catalog);
    // The Dutch and French titles and the Dutch tags; every English
    // description, name and tag list is skipped.
    assert_eq!(
        plan.pending
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([CBS_TITLE, INSEE, TAGS])
    );
    assert_eq!(plan.english + plan.pending.len(), plan.units);
    let generation = catalog.english.generation();
    let report = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(english.translate(plan.pending));
    assert_eq!((report.translated, report.failed, report.calls), (3, 0, 1));
    assert!(catalog.english.generation() > generation);
    assert_eq!(catalog.english.english(INSEE).as_deref(), Some(INSEE_EN));
    assert_eq!(
        catalog.english.render(&["CBS Statistics", CBS_TITLE]),
        CBS_TITLE_EN
    );
    // A refresh of the same catalog translates nothing again.
    let again = english.plan(&catalog);
    assert!(again.pending.is_empty());
    assert_eq!(again.cached, 3);
    assert_eq!(fake.calls(), 1);
}

#[test]
fn texts_are_sent_in_bounded_batches() {
    let texts: Vec<String> = (0..7)
        .map(|n| format!("Werkloosheid in gemeente {n}"))
        .collect();
    let fake = Arc::new(Fake::new(&[]));
    let english = English::new(fake.clone(), limits());
    let report = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(english.translate(texts));
    assert_eq!(*fake.batches.lock().unwrap(), vec![3, 3, 1]);
    // Nothing in the dictionary: nothing remembered, all left for later.
    assert_eq!((report.translated, report.failed), (0, 7));
}

#[tokio::test]
async fn an_unavailable_model_is_retried_and_a_refusal_is_not() {
    let pending = vec![CBS_TITLE.to_owned()];
    let fake =
        Arc::new(Fake::new(&dictionary()).failing(&[Failure::Unavailable, Failure::Unavailable]));
    let english = English::new(fake.clone(), limits());
    let report = english.translate(pending.clone()).await;
    assert_eq!((report.translated, report.calls), (1, 3));

    let fake = Arc::new(Fake::new(&dictionary()).failing(&[Failure::Refused(400)]));
    let english = English::new(fake.clone(), limits());
    let report = english.translate(pending).await;
    assert_eq!((report.translated, report.failed, report.calls), (0, 1, 1));
    // A failed text waits for its backoff instead of being sent at every load.
    let mut catalog = mixed();
    catalog.english = english.memory();
    let plan = english.plan(&catalog);
    assert_eq!(plan.backing_off, 1);
    assert!(!plan.pending.iter().any(|text| text == CBS_TITLE));
}

#[tokio::test]
async fn a_refused_key_ends_the_pass() {
    let texts: Vec<String> = (0..9)
        .map(|n| format!("Werkloosheid in gemeente {n}"))
        .collect();
    let fake = Arc::new(Fake::new(&[]).failing(&[Failure::Refused(401)]));
    let english = English::new(fake.clone(), limits());
    let report = english.translate(texts).await;
    assert_eq!(fake.calls(), 1);
    assert_eq!(report.failed, 9);
}

#[tokio::test]
async fn attaching_a_catalog_translates_it_in_the_background() {
    let fake = Arc::new(Fake::new(&dictionary()));
    let english = Arc::new(English::new(fake.clone(), limits()));
    let mut catalog = mixed();
    english.attach(&mut catalog);
    assert_eq!(catalog.english.id(), english.memory().id());
    let deadline = Instant::now() + Duration::from_secs(5);
    while catalog.english.len() < 3 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(catalog.english.len(), 3);
    // A refreshed catalog shares the memory: nothing is sent again.
    let mut refreshed = mixed();
    english.attach(&mut refreshed);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fake.calls(), 1);
    assert_eq!(
        refreshed.english.english(CBS_TITLE).as_deref(),
        Some(CBS_TITLE_EN)
    );
}

#[test]
fn without_translation_the_catalog_is_left_as_it_is() {
    let english = Arc::new(English::disabled());
    let mut catalog = mixed();
    english.attach(&mut catalog);
    assert!(catalog.english.is_empty());
    assert!(!english.enabled());
    for (value, on) in [
        (None, true),
        (Some("1"), true),
        (Some("true"), true),
        (Some("0"), false),
        (Some("false"), false),
        (Some(" OFF "), false),
        (Some("no"), false),
    ] {
        assert_eq!(enabled(value), on, "{value:?}");
    }
    assert!(!English::from_env(reqwest::Client::new(), " ").enabled());
}

fn parsed(language: &str, name: &str, description: &str, outputs: &[&str]) -> ParsedRequest {
    normalize(
        &json!({"language":language,"subjects":[{"id":"s1","label":"Nederland","type":"country","identifiers":{}}],
            "capabilities":[{"slug":"research.statistics.unemployment","name":name,"description":description,"domain":"research",
                "inputs":["land"],"outputs":outputs,"subject_id":"s1","source_hint":null,"confidence":0.9}],
            "deliverable":{"format":"chat","operations":[]}})
        .to_string(),
        "Werkloosheid in Nederland",
        "fixture",
    )
    .unwrap()
}

#[tokio::test]
async fn a_capability_that_is_not_english_is_also_read_in_english() {
    let dutch = parsed(
        "nl",
        "Werkloosheid",
        "Geeft het werkloosheidspercentage van een land per maand.",
        &["werkloosheidspercentage", "periode"],
    );
    let fake = Arc::new(Fake::new(&[
        ("Werkloosheid", "Unemployment"),
        (
            "Geeft het werkloosheidspercentage van een land per maand.",
            "Returns the unemployment rate of a country per month.",
        ),
        ("land", "country"),
        (
            "werkloosheidspercentage, periode",
            "unemployment rate, period",
        ),
    ]));
    let english = English::new(fake.clone(), limits());
    let normalized = english.request(&dutch).await;
    let capability = &normalized.english.capabilities[0];
    assert_eq!(capability.name, "Unemployment");
    assert_eq!(capability.inputs, ["country"]);
    assert_eq!(capability.outputs, ["unemployment_rate", "period"]);
    assert_eq!(capability.slug, dutch.capabilities[0].slug);
    assert_eq!(normalized.original.capabilities[0].name, "Werkloosheid");
    assert_eq!(fake.calls(), 1);
    // The same capability again (the App searches Apiosk and Bazaar): cached.
    english.request(&dutch).await;
    assert_eq!(fake.calls(), 1);
    // An English parse, whatever the question's language, costs no call.
    let already = parsed(
        "nl",
        "Unemployment rate",
        "Retrieves the unemployment rate of a country for each month.",
        &["unemployment_rate"],
    );
    let normalized = english.request(&already).await;
    assert_eq!(normalized.english.capabilities[0].name, "Unemployment rate");
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn a_failed_or_slow_translation_searches_the_request_as_it_came() {
    let dutch = parsed(
        "nl",
        "Werkloosheid",
        "Geeft het werkloosheidspercentage van een land per maand.",
        &["periode"],
    );
    for fake in [
        Fake::new(&[]).failing(&[Failure::Unavailable]),
        Fake::new(&[("Werkloosheid", "Unemployment")]).slow(Duration::from_secs(5)),
    ] {
        let english = English::new(Arc::new(fake), limits());
        let started = Instant::now();
        let normalized = english.request(&dutch).await;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(normalized.english.capabilities[0].name, "Werkloosheid");
        assert_eq!(
            normalized.english.capabilities[0].description,
            dutch.capabilities[0].description
        );
    }
    // Without a translator nothing is sent at all.
    let normalized = English::disabled().request(&dutch).await;
    assert_eq!(normalized.english.capabilities[0].name, "Werkloosheid");
}

#[tokio::test]
async fn a_late_translation_serves_the_next_search_of_the_same_wording() {
    let dutch = parsed(
        "nl",
        "Werkloosheid",
        "Geeft het werkloosheidspercentage van een land per maand.",
        &["periode"],
    );
    let fake = Arc::new(Fake::new(&[("Werkloosheid", "Unemployment")]).slow(Duration::from_millis(800)));
    let english = English::new(fake.clone(), limits());
    // Past the deadline: this search reads the wording as it came...
    assert_eq!(english.request(&dutch).await.english.capabilities[0].name, "Werkloosheid");
    // ...and the answer, once in, is the next one's, without another call.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(english.request(&dutch).await.english.capabilities[0].name, "Unemployment");
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn only_bazaar_listings_in_another_language_are_translated() {
    let listings = vec![
        "Stock Trends. Compares two user-supplied portfolios using decision scoring.".to_owned(),
        "Insolvenzen. Beantragte Unternehmensinsolvenzen nach Wirtschaftszweigen und Monaten."
            .to_owned(),
    ];
    let fake = Arc::new(Fake::new(&[(
        listings[1].as_str(),
        "Insolvencies. Company insolvency applications by economic sector and month.",
    )]));
    let english = English::new(fake.clone(), limits());
    let rendered = english.listings(&listings).await;
    assert_eq!(rendered.len(), 1);
    assert!(rendered[&listings[1]].starts_with("Insolvencies."));
    assert_eq!(*fake.batches.lock().unwrap(), vec![1]);
}

#[test]
fn search_finds_a_foreign_listing_through_its_english_rendering_and_falls_back_without_it() {
    let request = normalize(
        &json!({"language":"en","subjects":[],"capabilities":[{"slug":"research.statistics.unemployment","name":"Unemployment rate",
            "description":"Retrieves the unemployment rate of a country over time.","domain":"research","inputs":["country"],
            "outputs":["unemployment_rate","period"],"subject_id":null,"source_hint":null,"confidence":0.9}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(),
        "Unemployment rate",
        "fixture",
    )
    .unwrap();
    let offered = |catalog: &Catalog| -> Vec<String> {
        crate::ask_v2::search::search(&request, catalog)[0]
            .apiosk
            .iter()
            .filter_map(|candidate| candidate.capability.clone())
            .collect()
    };
    let plain = mixed();
    let before = offered(&plain);
    assert!(!before.iter().any(|capability| capability.contains("chomage") || capability.contains("werkloosheid")), "{before:?}");
    let mut translated = mixed();
    translate_catalog(&mut translated, &dictionary());
    let after = offered(&translated);
    assert!(
        after.contains(&"operation.insee.get-chomage".to_owned()),
        "{after:?}"
    );
    assert!(
        after.contains(&"operation.cbs.get-werkloosheid".to_owned()),
        "{after:?}"
    );
    // Every original match is kept.
    assert!(before.iter().all(|capability| after.contains(capability)));
}

mod qwen {
    use super::*;
    use axum::{http::StatusCode, routing::post, Json, Router};

    async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (endpoint, task)
    }
    fn completion(content: &str) -> Value {
        json!({"provider":"Alibaba","choices":[{"message":{"content":content}}],"usage":{"prompt_tokens":10,"completion_tokens":5}})
    }

    #[tokio::test]
    async fn sends_apiosks_one_model_a_json_object_request_and_reads_renderings_by_id() {
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = captured.clone();
        let app = Router::new().route("/", post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer fixture-key");
                seen.lock().unwrap().push(body);
                Json(completion(r#"{"texts":[{"id":"2","english":"ILO unemployment rate"},{"id":"1","english":"Unemployment; key figures"},{"id":"9","english":"stray"}]}"#))
            }
        }));
        let (endpoint, server) = server(app).await;
        let qwen = Qwen::new(reqwest::Client::new(), "fixture-key".into())
            .with_local_endpoint(endpoint)
            .unwrap();
        let texts = vec![CBS_TITLE.to_owned(), INSEE.to_owned(), TAGS.to_owned()];
        let english = qwen.translate(&texts).await.unwrap();
        assert_eq!(
            english,
            vec![
                Some("Unemployment; key figures".into()),
                Some("ILO unemployment rate".into()),
                None
            ]
        );
        let body = captured.lock().unwrap()[0].clone();
        assert_eq!(body["model"], "qwen/qwen3-30b-a3b-instruct-2507");
        assert_eq!(body["model"], crate::parser::MODEL);
        assert_eq!(body["response_format"], json!({"type":"json_object"}));
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["messages"][0]["content"], PROMPT);
        let sent: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            sent,
            json!({"texts":[{"id":"1","text":CBS_TITLE},{"id":"2","text":INSEE},{"id":"3","text":TAGS}]})
        );
        server.abort();
    }

    #[tokio::test]
    async fn refusals_unavailability_and_invalid_replies_are_told_apart() {
        let app = Router::new()
            .route(
                "/bad",
                post(|| async { (StatusCode::BAD_REQUEST, "private provider detail") }),
            )
            .route(
                "/busy",
                post(|| async { (StatusCode::TOO_MANY_REQUESTS, "slow down") }),
            )
            .route(
                "/down",
                post(|| async { (StatusCode::BAD_GATEWAY, "down") }),
            )
            .route(
                "/prose",
                post(|| async { Json(completion("Here is the translation: Unemployment")) }),
            )
            .route(
                "/empty",
                post(|| async { Json(completion(r#"{"texts":[{"id":"1","english":""}]}"#)) }),
            );
        let (endpoint, server) = server(app).await;
        let texts = vec![CBS_TITLE.to_owned()];
        for (path, expected) in [
            ("bad", Failure::Refused(400)),
            ("busy", Failure::Unavailable),
            ("down", Failure::Unavailable),
            ("prose", Failure::Invalid),
            ("empty", Failure::Invalid),
        ] {
            let qwen = Qwen::new(reqwest::Client::new(), "fixture-key".into())
                .with_local_endpoint(format!("{endpoint}{path}"))
                .unwrap();
            assert_eq!(qwen.translate(&texts).await, Err(expected), "{path}");
        }
        assert!(Qwen::new(reqwest::Client::new(), "k".into())
            .with_local_endpoint("https://example.com/".into())
            .is_err());
        server.abort();
    }
}
