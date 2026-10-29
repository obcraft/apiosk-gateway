//! `GET /v1/ask?q=...` — van vraag naar de vijf endpoints die haar beantwoorden.
//!
//! Een nieuw endpoint naast `/v1/discover`, niet in plaats daarvan. `/v1/discover`
//! dient de bestaande pijplijn (compare, decide, do) met zijn lanes, zijn
//! `capability`-contract en zijn externe kandidaten. Dit endpoint doet één ding:
//! zo snel mogelijk vijf bruikbare endpoints met uitleg teruggeven.
//!
//! ```text
//!   vraag
//!     │ 1. lezen        één modelcall  →  geparseerd object
//!     │ 2. voorfilteren catalogus + open indexen, in het geheugen
//!     │                                 →  shortlist van 60   (1 query)
//!     │ 3. herordenen   één modelcall  →  top 5 met reden en `fit`
//!     │ 4. de lat       beste fit < `MATCH_FLOOR` → `needs_context`
//!     ▼
//!   antwoord, of één vraag terug
//! ```
//!
//! Twee modelcalls, allebei op een klein snel model. Stap 2 is bijna gratis:
//! catalogus en gecrawlde indexen staan in het proces, dus de enige query
//! vraagt welke externe hosts de gateway mag betalen. Zonder sleutel valt
//! alles terug op de lexicale volgorde: minder scherp, en nog een antwoord.
//!
//! De open indexen zitten in DEZELFDE pool als de catalogus, anders dan bij
//! `/v1/discover`. Zie [`external_offers`] voor wat er onderweg afvalt.
//!
//! Stap 4 kost geen call. Het model uit stap 3 gaf per pick al een `fit`, en
//! wanneer niets de lat haalt heeft het ook al opgeschreven wat de steller nog
//! moet vertellen (`context_question`). Zie [`ask::MATCH_FLOOR`] voor waarom
//! de lat op de fit ligt en niet op de lexicale relevantie.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ask::{self, Selected};
use crate::AppState;

// De time-outs staan in `config` (`ASK_READ_TIMEOUT_MS`, `ASK_RERANK_TIMEOUT_MS`),
// naast de modelknoppen die bepalen of ze ruim genoeg zijn.

const MAX_QUESTION_CHARS: usize = 1_000;

#[derive(Debug, Deserialize)]
pub struct AskQuery {
    /// De vraag. `q` en `question` doen hetzelfde; agents gebruiken beide.
    #[serde(alias = "question")]
    pub q: Option<String>,
    /// Hoeveel endpoints er terug moeten. Standaard vijf.
    pub limit: Option<usize>,
    /// De lat die de beste `fit` moet halen voordat het antwoord als zeker
    /// geldt; daaronder komt `needs_context` mee. Standaard
    /// [`ask::MATCH_FLOOR`]; 0–100, daarbuiten afgekapt.
    pub min_match: Option<i64>,
    /// Harde bovengrens op de prijs die de koper betaalt. Kandidaten boven de
    /// grens bereiken de shortlist en het model niet, zodat prijsfiltering de
    /// canonieke keuze niet achteraf in een andere keuze verandert.
    pub max_price: Option<f64>,
    /// `debug=1` voegt `meta.debug` toe: de shortlist zoals het model hem te
    /// zien kreeg en het ruwe herordeningsantwoord. Voor het tunen van de
    /// stap; kost niets extra.
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub debug: bool,
}

#[derive(Debug, Serialize)]
pub struct AskResponse {
    pub question: String,
    /// Het geparseerde object. `null` als er geen parser geconfigureerd is.
    pub parsed: Option<Value>,
    /// De endpoints, beste eerst.
    pub endpoints: Vec<Selected>,
    /// Gezet zodra de bovenste keuze een verplicht veld eist dat de vraag niet
    /// geeft: welke velden dat zijn, en één vraag aan de steller om ze op te
    /// halen. `null` wanneer de vraag al genoeg zegt om te kunnen uitvoeren.
    ///
    /// Het is expliciet GEEN foutstatus. De endpoints staan er gewoon bij: dit
    /// zegt "dit is de bron, en dit heb ik nog van je nodig", niet "probeer het
    /// opnieuw".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_input: Option<ask::NeedsInput>,
    /// Gezet zodra geen endpoint de lat haalt (`meta.confident: false`) of er
    /// helemaal geen bron is (`meta.found: false`): één vraag aan de steller om
    /// de context die een bronkeuze mogelijk maakt, plus de labels waar die
    /// vraag om gaat. De endpoints die er wél zijn staan er gewoon bij, met hun
    /// `fit`, zodat wie de zwakke keuze toch wil hem kan zien.
    ///
    /// Sluit `needs_input` uit: één vraag per antwoord. Een veld opvragen voor
    /// een endpoint waar we niet achter staan is de verkeerde vraag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_context: Option<ask::NeedsContext>,
    pub meta: Value,
}

/// Waar de fases heen gaan terwijl het antwoord gebouwd wordt.
///
/// `None` voor een gewone JSON-aanroep. Voor een SSE-aanroep de zender van
/// het kanaal dat de stream voedt: elke fase is één `data:`-regel met een
/// `phase`, en de laatste draagt het hele antwoord. Een gesloten ontvanger
/// (de client is weg) maakt `send` een no-op; de fase kost dan niets.
#[derive(Clone, Default)]
pub struct Progress(Option<tokio::sync::mpsc::UnboundedSender<Value>>);

impl Progress {
    fn send(&self, event: Value) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(event);
        }
    }
}

/// `GET /v1/ask`. Eén vorm, twee transporten.
///
/// Met `Accept: text/event-stream` komen de fases (`read`, `shortlist`,
/// `rerank`) als server-sent events terwijl ze gebeuren, en het antwoord als
/// laatste event (`phase: "answer"`). Zonder komt hetzelfde antwoord als één
/// JSON-body. De App streamt; agents en curl lezen JSON. Zelfde beslissing,
/// zelfde velden.
/// Which engine produced this answer, stamped on every path out of `run_ask`.
///
/// The App must not read a build-time flag to know this. Netlify inlines
/// `VITE_*` variables at build time, so a flag the browser reads can only be
/// changed by rebuilding and redeploying — which is not a rollback. The engine
/// is therefore reported by whoever actually answered, and turning the rework
/// off stays `fly secrets unset ASK_ENGINE_V2` with nothing to rebuild.
fn stamp_engine(mut answer: AskResponse, engine: &'static str) -> AskResponse {
    if let Some(meta) = answer.meta.as_object_mut() {
        meta.insert("engine".to_string(), Value::String(engine.to_string()));
    }
    answer
}

pub async fn ask(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<AskQuery>,
) -> Response {
    let streaming = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    // Read before `state` moves into `run_ask`, and `'static` so the streaming
    // branch can carry it into the spawned task.
    let engine: &'static str = if state.config.ask_engine_v2 {
        "v2"
    } else {
        "v1"
    };

    if !streaming {
        return match run_ask(state, params, Progress::default()).await {
            Ok(body) => Json(stamp_engine(body, engine)).into_response(),
            Err((status, body)) => (status, body).into_response(),
        };
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let progress = Progress(Some(tx.clone()));
    tokio::spawn(async move {
        let last = match run_ask(state, params, progress).await {
            Ok(answer) => {
                json!({ "phase": "answer", "answer": stamp_engine(answer, engine) })
            }
            Err((status, Json(body))) => {
                json!({ "phase": "error", "status": status.as_u16(), "error": body })
            }
        };
        let _ = tx.send(last);
    });

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|value| {
            (
                Ok::<Event, Infallible>(Event::default().data(value.to_string())),
                rx,
            )
        })
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn run_ask(
    state: Arc<AppState>,
    params: AskQuery,
    progress: Progress,
) -> Result<AskResponse, (StatusCode, Json<Value>)> {
    let started = Instant::now();

    let question = params
        .q
        .unwrap_or_default()
        .trim()
        .chars()
        .take(MAX_QUESTION_CHARS)
        .collect::<String>();

    if question.len() < 3 {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "q is required",
                "hint": "GET /v1/ask?q=who owns Mollie and is the company healthy",
            })),
        ));
    }

    let limit = params.limit.unwrap_or(ask::TOP_N).clamp(1, 10);
    let floor = match_floor(params.min_match);

    // --- 1. lezen -------------------------------------------------------------------
    let read_started = Instant::now();
    let parsed = read_question(&state, &question).await;
    let read_ms = read_started.elapsed().as_millis() as u64;
    progress.send(json!({ "phase": "read", "parsed": parsed, "took_ms": read_ms }));

    // --- 2. voorfilteren ------------------------------------------------------------
    let snapshot = match state
        .cache
        .get_ask_snapshot(&state.db, state.config.ask_engine_v2)
        .await
    {
        Ok(s) => s,
        Err(err) => {
            tracing::error!("ask: catalogue unavailable: {err}");
            return Err((
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "catalogue unavailable" })),
            ));
        }
    };

    let terms = match &parsed {
        Some(p) => ask::search_terms(&question, p),
        // Zonder parser is de vraag zelf de zoekterm. Dat is precies wat
        // discovery deed voordat er een parser was.
        None => ask::Terms {
            core: ask::terms(&question),
            expansion: Vec::new(),
        },
    };

    let shortlist_started = Instant::now();
    // Reliability is a property of the canonical decision, not a client-side
    // afterthought. App and MCP both consume this exact order, so filtering a
    // failing listing here prevents either caller from recommending it or
    // inventing a different fallback. A cold health cache leaves listings in
    // the field; only measured quarantine excludes one.
    let provider_stats = state.cache.peek_provider_stats().await.unwrap_or_default();
    let mut eligible: Vec<crate::db::CapabilityOffer> = snapshot
        .iter()
        .filter(|offer| {
            eligible_for_ask(offer, provider_stats.get(&offer.api_slug), params.max_price)
        })
        .cloned()
        .collect();
    // The open indexes in the SAME pool as the catalogue. `/v1/discover` keeps
    // them apart; this endpoint asks one question and wants the best answer.
    let reach = state.source_index.reach().await;
    let external = external_offers(&state, params.max_price).await;
    let external_count = external.len();
    eligible.extend(external);

    // ONE ruler, applied ONCE, over everything Apiosk can reach.
    //
    // The pool used to arrive pre-cut: the index sweep answered with at most
    // `MAX_MATCHES_PER_SOURCE` (400) listings per source, chosen by a substring
    // match, and only those reached the scoring below. Two different rankers
    // cutting in sequence meant the second one could not recover what the
    // first had dropped, and neither of them said so — `meta.considered` read
    // 1,653 while `meta.searched.endpoints` read 3,610, and nothing named the
    // ~2,000 endpoints in between as unreachable.
    let weights = ask::Weights::over(&eligible, &terms);
    let ask::Shortlist { pairs, matched } =
        ask::shortlist_with_matches(&eligible, &terms, &weights, ask::SHORTLIST);
    let shortlist_ms = shortlist_started.elapsed().as_millis() as u64;
    progress.send(json!({
        "phase": "shortlist",
        "shortlisted": pairs.len(),
        // Het getal dat met de vraag meebeweegt; `shortlisted` is de limiet.
        "matched": matched,
        "considered": eligible.len(),
        "searched": searched(&snapshot, &reach),
        "took_ms": shortlist_ms,
    }));

    // De kandidaten en hun rijen lopen vanaf hier in lockstep: de `id` die het
    // model noemt is de index in beide. Eerder werd de rij teruggezocht op
    // resource-URL, en dat brak zodra twee rijen dezelfde aanroep deelden.
    let candidates: Vec<ask::Candidate> = pairs.iter().map(|(c, _)| c.clone()).collect();
    let ordered: Vec<&crate::db::CapabilityOffer> = pairs.iter().map(|(_, o)| *o).collect();

    // Niets dat ook maar in de buurt komt: dan hoeft er geen modelcall te
    // gebeuren om dat te bevestigen. Zie [`ask::RELEVANCE_FLOOR`] — dit is een
    // zeef, niet het oordeel; dat valt na het herordenen.
    let best = ask::best_relevance(&pairs);
    if pairs.is_empty() || best < ask::RELEVANCE_FLOOR {
        // Geen model gedraaid, dus de vraag terug komt van de lezer of van
        // het sjabloon. Dat is minder scherp dan wat het herordeningsmodel
        // had kunnen zeggen, en nog steeds beter dan alleen "geen bron".
        let context =
            ask::needs_context("no_source", best, floor, None, parsed.as_ref(), &question);
        let mut response = no_source(
            question,
            parsed,
            if pairs.is_empty() {
                "Nothing in the catalogue or the open indexes matches this request."
            } else {
                "Nothing in the catalogue or the open indexes comes close enough to this request."
            },
            best,
            context,
            &snapshot,
            &reach,
            &terms,
            started,
        );
        if params.debug {
            response.meta["debug"] = json!({ "shortlist": candidates, "rerank": Value::Null });
        }
        return Ok(response);
    }

    // --- 3. herordenen --------------------------------------------------------------
    progress.send(json!({ "phase": "rerank", "candidates": candidates.len() }));
    let rerank_started = Instant::now();
    let (mut endpoints, ranked_by, follow_up, verdict) = match rerank(
        &state,
        &question,
        parsed.as_ref(),
        &candidates,
        limit,
    )
    .await
    {
        Some(raw) => {
            // HET MODEL MAG NEE ZEGGEN, en dat is het hele punt van deze tak.
            //
            // Een lege lijst van een model dat de kandidaten gelezen heeft,
            // is een oordeel: geen van deze endpoints beantwoordt de vraag.
            // Dat is iets anders dan een mislukte call, en het onderscheid is
            // load-bearing — terugvallen op de lexicale volgorde zou hier
            // precies de vijf zwakke kaarten tonen die we niet willen tonen.
            //
            // Het model heeft de kandidaten gezien, dus zijn contextvraag is
            // de beste die er is: die kan zeggen wat de shortlist wél dekt.
            if ask::said_no(&raw) {
                let context = ask::needs_context(
                    "no_source",
                    best,
                    floor,
                    Some(&raw),
                    parsed.as_ref(),
                    &question,
                );
                let mut response = no_source(
                    question,
                    parsed,
                    "The catalogue and the open indexes were searched and read; none of their sources answers this request.",
                    best,
                    context,
                    &snapshot,
                    &reach,
                    &terms,
                    started,
                );
                if params.debug {
                    response.meta["debug"] = json!({ "shortlist": candidates, "rerank": raw });
                }
                return Ok(response);
            }

            let picked = ask::read_rerank(&raw, &candidates, &ordered, limit);
            if picked.is_empty() {
                (
                    ask::without_rerank(&candidates, &ordered, limit),
                    "lexical",
                    None,
                    None,
                )
            } else {
                // De vervolgvraag hoort bij de keuze die het model maakte,
                // dus hij wordt tegen de BOVENSTE kandidaat gehouden — niet
                // tegen de shortlist, waar altijd wel íets een veld eist.
                let top = picked
                    .first()
                    .and_then(|s| candidates.iter().find(|c| c.resource == s.resource));
                let follow_up = ask::read_follow_up(&raw, top);
                (picked, "model", follow_up, Some(raw))
            }
        }
        None => (
            ask::without_rerank(&candidates, &ordered, limit),
            "lexical",
            None,
            None,
        ),
    };
    let rerank_ms = rerank_started.elapsed().as_millis() as u64;

    // The semantic decision above is final. Add the execution details to the
    // SAME selected rows; clients must not run a second discovery/ranking pass
    // merely to obtain a schema or a pre-step price.
    let mut executable = endpoints
        .iter()
        .map(|endpoint| endpoint.candidate.clone())
        .collect::<Vec<_>>();
    super::flow::annotate_pre_steps(&state, &mut executable, &question).await;
    for (endpoint, candidate) in endpoints.iter_mut().zip(executable) {
        endpoint.candidate = candidate;
    }

    // --- 4. de lat -------------------------------------------------------------------
    //
    // Geen call: de `fit` per pick kwam mee uit stap 3. Haalt de beste hem
    // niet, dan is het antwoord een vraag terug en géén vervolgvraag om een
    // veld — één vraag per antwoord, en die moet over de keuze gaan die nog
    // niet gemaakt is. Zie [`ask::MATCH_FLOOR`].
    let debug = params.debug.then(|| {
        json!({
            "shortlist": candidates,
            "rerank": verdict,
        })
    });
    let top_fit = ask::best_fit(&endpoints);
    let (score, scored_by) = ask::match_score(top_fit, best);
    let confident = top_fit.is_some_and(|fit| fit >= floor);

    // TWEE SCHALEN, ÉÉN LAT — DAT KAN NIET, EN DIT IS WAAR HET MISGING.
    //
    // `floor` is geijkt op de `fit` van het herordeningsmodel: 75 scheidt "dit
    // beantwoordt de kern" van "dit komt in de buurt". Draaide er geen model
    // (geen sleutel, een time-out), dan is `score` de LEXICALE relevantie, en
    // die schaal draagt geen oordeel — zie [`ask::MATCH_FLOOR`], waar een
    // goede vraag 39 scoort en een onbeantwoordbare 42.
    //
    // Wat gemeten verkeer laat zien: het KVK-register scoort op "Where is
    // Mollie B.V. registered?" fit 90 en relevantie 59. Eén mislukte
    // modelcall maakte van precies dat antwoord een `weak_match`, en de
    // steller kreeg een formulier voor een vraag waar niets mis mee was.
    //
    // Zonder model weten we niet dát de keuze zwak is — we weten alleen dat we
    // hem niet beoordeeld hebben. `confident` blijft daarom `false` en
    // `match_scored_by` zegt "lexical", maar om context vragen doen we niet:
    // dat legt ONZE onzekerheid bij de steller neer als een gebrek in ZIJN
    // vraag. De picks dragen zelf al "not ranked by a model".
    let (needs_input, needs_context) = if !ask::asks_for_context(top_fit, score, floor) {
        (follow_up, None)
    } else {
        (
            None,
            Some(ask::needs_context(
                "weak_match",
                score,
                floor,
                verdict.as_ref(),
                parsed.as_ref(),
                &question,
            )),
        )
    };

    let mut response = AskResponse {
        question,
        parsed,
        endpoints,
        needs_input,
        needs_context,
        meta: json!({
            "found": true,
            // Of de bovenste keuze de lat haalt. `false` betekent: de
            // endpoints staan er, maar vraag eerst wat `needs_context` vraagt.
            "confident": confident,
            "match_score": score,
            "match_scored_by": scored_by,
            "match_floor": floor,
            "top_fit": top_fit,
            "ranked_by": ranked_by,
            "ranking_status": if top_fit.is_some() { "evaluated" } else { "unavailable" },
            // Wat het herordeningsmodel de steller zou zeggen, in diens taal.
            // De App zet dit boven de kaarten. Afwezig zonder model.
            "summary": verdict.as_ref().and_then(ask::read_summary),
            // Welk model las en welk model oordeelde. Twee, want ze mogen
            // verschillen: zie `ASK_READ_MODEL` / `ASK_RERANK_MODEL`.
            "model": {
                "read": crate::query_plan::ask_read_vendor(&state.config).map(|v| v.label),
                "rerank": crate::query_plan::ask_rerank_vendor(&state.config).map(|v| v.label),
            },
            "top_relevance": best,
            "relevance_floor": ask::RELEVANCE_FLOOR,
            // Nooit nul zolang de catalogus gevuld is, en dat is het punt:
            // "0 bronnen doorzocht" is een bewering over ons, niet over de vraag.
            "searched": searched(&snapshot, &reach),
            "shortlisted": candidates.len(),
            "matched": matched,
            "considered": eligible.len(),
            "external_considered": external_count,
            "timing_ms": {
                "read": read_ms,
                "shortlist": shortlist_ms,
                "rerank": rerank_ms,
                "total": started.elapsed().as_millis() as u64,
            },
        }),
    };
    if let Some(debug) = debug {
        response.meta["debug"] = debug;
    }
    Ok(response)
}

/// Het antwoord "we hebben hier geen bron voor".
///
/// Eén functie voor de twee plaatsen waar dat kan blijken — vóór het model
/// (niets komt in de buurt) en erna (het model heeft gelezen en zegt nee) —
/// zodat beide hetzelfde antwoord geven en `searched` in allebei het ECHTE
/// aantal draagt. "0 bronnen doorzocht" is een bewering over ons, niet over de
/// vraag, en mag daarom nergens uit dit endpoint komen.
#[allow(clippy::too_many_arguments)]
fn no_source(
    question: String,
    parsed: Option<Value>,
    reason: &str,
    best: u8,
    context: ask::NeedsContext,
    snapshot: &ask::Snapshot,
    reach: &crate::source_index::Reach,
    terms: &ask::Terms,
    started: Instant,
) -> AskResponse {
    let floor = context.floor;
    AskResponse {
        question,
        parsed,
        endpoints: Vec::new(),
        needs_input: None,
        needs_context: Some(context),
        meta: json!({
            "found": false,
            "confident": false,
            "reason": reason,
            "best_relevance": best,
            "relevance_floor": ask::RELEVANCE_FLOOR,
            "match_floor": floor,
            "searched": searched(snapshot, reach),
            "terms": terms.all(),
            "took_ms": started.elapsed().as_millis() as u64,
        }),
    }
}

/// Wat er doorzocht is, op drie niveaus.
///
/// Alle drie, want alle drie zijn waar en ze verschillen met een factor tien:
/// zes BRONNEN (PulseNetwork is er één), tachtig LISTINGS (elke Pulse-API is er
/// één) en elfhonderd ENDPOINTS. Eén getal noemen zonder te zeggen welk, is de
/// snelste manier om iemand te laten denken dat er iets ontbreekt.
/// Het account waar Apiosk zijn eigen wederverkoop onder hangt.
///
/// Het is een container, geen leverancier: KVK en StableEnrich staan er allebei
/// onder en hebben niets met elkaar te maken. Erop groeperen telde die twee als
/// één bron en maakte van zes bronnen vijf.
const PLATFORM_ACCOUNT: &str = "apiosk-basics";

/// Waarop twee listings dezelfde BRON zijn.
///
/// Normaal het provideraccount — PulseNetwork is zesenzeventig listings en één
/// bron. Behalve onder het platformaccount: daar identificeert een listing zich
/// als zichzelf, want dat account is een verzamelbak.
fn source_key(offer: &crate::db::CapabilityOffer) -> &str {
    match offer.provider_username.as_deref() {
        Some(PLATFORM_ACCOUNT) | None => offer.api_slug.as_str(),
        Some(username) => username,
    }
}

/// De lat uit de query, of de standaard. Buiten 0–100 wordt afgekapt in
/// plaats van geweigerd: een aanroeper die 120 stuurt bedoelt "streng".
fn match_floor(asked: Option<i64>) -> u8 {
    asked
        .map(|v| v.clamp(0, 100) as u8)
        .unwrap_or(ask::MATCH_FLOOR)
}

/// Hard eligibility for the one canonical interpretation.
///
/// Kept as one pure gate so a future App or agent feature cannot accidentally
/// apply price, callability or reliability after the semantic pick and thereby
/// turn one decision into two.
fn eligible_for_ask(
    offer: &crate::db::CapabilityOffer,
    stats: Option<&crate::db::ProviderStats>,
    max_price: Option<f64>,
) -> bool {
    offer.call_url.is_some()
        && !stats.is_some_and(|stats| stats.is_quarantined())
        && within_ceiling(offer, max_price)
}

/// Of de prijsgrens ruimte laat voor dit aanbod. Eén functie voor allebei de
/// helften van de pool.
fn within_ceiling(offer: &crate::db::CapabilityOffer, max_price: Option<f64>) -> bool {
    let Some(ceiling) = max_price else {
        return true;
    };
    ceiling.is_finite() && ceiling > 0.0 && super::flow::buyer_price_usd(offer) <= ceiling
}

fn searched(snapshot: &ask::Snapshot, reach: &crate::source_index::Reach) -> serde_json::Value {
    let mut providers: Vec<&str> = snapshot.iter().map(source_key).collect();
    providers.sort_unstable();
    providers.dedup();

    let mut listings: Vec<&str> = snapshot.iter().map(|o| o.api_slug.as_str()).collect();
    listings.sort_unstable();
    listings.dedup();

    json!({
        // Catalogus en indexen bij elkaar; apart eronder.
        "sources": providers.len() + reach.providers,
        "listings": listings.len() + reach.providers,
        "endpoints": snapshot.len() + reach.endpoints,
        "catalogue": {
            "sources": providers.len(),
            "listings": listings.len(),
            "endpoints": snapshot.len(),
        },
        "indexes": {
            "sources": reach.sources,
            "sellers": reach.providers,
            "endpoints": reach.endpoints,
            // Named, so a screen can say WHICH directories it just read.
            "names": reach.names,
        },
    })
}

/// De open indexen, uit het geheugen doorzocht op dezelfde termen als de
/// catalogus. Betaalbaarheid beslist hier, vóór het model: zie
/// [`eligible_for_ask`] — één beslissing, niet twee.
async fn external_offers(
    state: &Arc<AppState>,
    max_price: Option<f64>,
) -> Vec<crate::db::CapabilityOffer> {
    // Everything, not a sweep. See `SourceIndex::all` for why the term match
    // moved out of here and into the one scoring pass that follows it.
    let matched = state.source_index.all().await;
    if matched.is_empty() {
        return Vec::new();
    }
    let settlements = crate::proxy::external_fetch::quote_external_offers(state, &matched).await;
    matched
        .iter()
        .zip(settlements)
        .filter(|(_, settlement)| settlement.payable)
        .map(|(offer, _)| ask::as_offer(offer))
        .filter(|offer| within_ceiling(offer, max_price))
        .collect()
}

/// Stap 1: de vraag lezen. `None` als er geen parser is of hij faalt.
pub(crate) async fn read_question(state: &Arc<AppState>, question: &str) -> Option<Value> {
    // De capability-lijst komt live uit de catalogus, zodat een capability die
    // vanochtend is toegevoegd vanmiddag gekozen kan worden.
    let capabilities = state
        .db
        .list_capabilities()
        .await
        .map(|caps| {
            caps.into_iter()
                .map(|c| c.slug)
                .take(200)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    read_question_among(state, question, capabilities).await
}

/// De vraag lezen met precies deze capabilities als keuze.
///
/// De researchplanner geeft hier alleen capabilities door waar een actief
/// uitvoeringscontract achter zit. Een lezer die uit de hele catalogus mag
/// kiezen, kiest geregeld een slug zonder contract — en dan wordt een vraag
/// die wél te beantwoorden is, teruggestuurd met "geen gevalideerde bron".
pub(crate) async fn read_question_among(
    state: &Arc<AppState>,
    question: &str,
    capabilities: Vec<String>,
) -> Option<Value> {
    let vendor = crate::query_plan::ask_read_vendor(&state.config)?;
    if capabilities.is_empty() {
        tracing::warn!("ask: reading skipped: no capabilities to choose from");
        return None;
    }

    let system = read_system_prompt(&capabilities);
    crate::query_plan::call_structured(
        state,
        &vendor,
        &system,
        question.to_string(),
        read_schema(&capabilities),
        1_600,
        "ask reading",
        Duration::from_millis(state.config.ask_read_timeout_ms),
    )
    .await
    .map_err(|err| tracing::warn!("ask: reading failed: {err}"))
    .ok()
}

/// Read a research question against the live catalogue: the kinds of subject
/// it can look up, the facts that identify each, and the capabilities with an
/// active contract. Nothing about companies lives here; a contract for a new
/// kind of subject changes the prompt and the schema on the next request.
pub(crate) async fn read_research_question(
    state: &Arc<AppState>,
    question: &str,
    catalogue: &crate::plan::reading::ReaderCatalogue,
) -> Option<Value> {
    let vendor = crate::query_plan::ask_read_vendor(&state.config)?;
    if catalogue.outputs_by_capability.is_empty() || catalogue.inputs.is_empty() {
        tracing::warn!("research reading skipped: no contracted capabilities or inputs");
        return None;
    }
    let system = research_system_prompt(catalogue);
    crate::query_plan::call_structured(
        state,
        &vendor,
        &system,
        question.to_string(),
        research_schema(catalogue),
        1_600,
        "research reading",
        Duration::from_millis(state.config.ask_read_timeout_ms),
    )
    .await
    .map_err(|err| tracing::warn!("research reading failed: {err}"))
    .ok()
}

fn research_system_prompt(catalogue: &crate::plan::reading::ReaderCatalogue) -> String {
    let kinds: Vec<String> = catalogue
        .kinds()
        .iter()
        .map(|kind| {
            let facts: Vec<String> = catalogue
                .inputs_of(kind)
                .iter()
                .map(|(slug, f)| match &f.pattern {
                    Some(p) => format!(
                        "{}: `{slug}` ({}; must match {p})",
                        catalogue.inputs.keys().position(|k| k == slug).unwrap(),
                        f.value_type
                    ),
                    None => format!(
                        "{}: `{slug}` ({})",
                        catalogue.inputs.keys().position(|k| k == slug).unwrap(),
                        f.value_type
                    ),
                })
                .collect();
            format!("- {kind}: known by {}", facts.join(", "))
        })
        .collect();
    let capabilities: Vec<String> = catalogue
        .outputs_by_capability
        .iter()
        .map(|(cap, facts)| {
            let required = catalogue
                .inputs_by_capability
                .get(cap)
                .into_iter()
                .flatten()
                .map(|set| {
                    format!(
                        "[{}]",
                        set.iter()
                            .filter_map(|slug| catalogue.inputs.keys().position(|k| k == slug))
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            format!(
                "- `{cap}` requires input indices {required}; yields {}",
                facts
                    .iter()
                    .enumerate()
                    .map(|(i, f)| format!("{i}: {f}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    format!(
        "You convert a research question into a strict execution object.\n\n\
         A subject is something the catalogue can look up. The kinds of subject, and the \
         facts a subject of that kind is known by, are exactly these:\n{}\n\n\
         The capabilities that can be run, and the facts each yields:\n{}\n\n\
         Rules:\n\
         - `question.text_en` is the question in English, translated as literally as possible. \
           Copy it verbatim when the question is already English. Proper nouns stay exactly as \
           the asker wrote them.\n\
         - `resolved_entities` holds one entry per distinct subject the question is about, with \
           `kind` from the list above and `name` as the asker wrote it. A question about two \
           subjects has two entries.\n\
         - `known` is a sparse array of {{fact_type, value}} entries. `fact_type` is the INTEGER index \
           printed before the input fact above, not its name. Include ONLY values that appear in the question \
           itself. Never supply an identifier, number or code from memory, not even for a \
           well-known subject: a value you recall becomes a paid lookup of the wrong one. \
           Omit unknown facts entirely. Emit each fact_type at most once. Encode value as a string; \
           for numbers, booleans and arrays use JSON text inside that string.\n\
         - Choose capabilities by the OUTPUTS the asker wants. Required inputs do not all need to \
           be present yet: the deterministic planner may obtain them from another contracted \
           capability (for example company name -> registry number -> profile). Include in `known` \
           only inputs literally present in the question. Never invent a missing input and never \
           replace it with a different fact of the same kind. If no contracted chain can provide it, \
           the server will ask for that exact typed input before any plan is approved.\n\
         - `country` is the ISO-3166 alpha-2 country of the subject when the question states \
           or clearly implies it, else null.\n\
         - One execution step per distinct sub-question, numbered from 1 in logical order; \
           `capability` must come from the list above, exactly, and `subject` names the entity \
           the step is about. `outputs` lists ONLY the INTEGER indices of facts needed to answer this sub-question, \
           using the indices in that capability's yields above. Never request every available fact.\n\
         - If one composed fact (such as a profile) answers the question, request that fact alone. \
           Do not add related fields or identifiers unless the user separately requested them.\n\
         - `expected_output` names what the asker wants back, one item per kind.\n\
         - `question.language` is the ISO-639-1 code of the language the asker wrote in.\n\
         - `question.recency_days` is the requested freshness window in whole days, or null.\n\
         - `question.clarifying_question`: when the question names no subject of a listed kind, \
           or no listed capability could answer it, write ONE short question in the asker's \
           language asking for exactly what is missing. Null when the question can be acted on.",
        kinds.join("\n"),
        capabilities.join("\n"),
    )
}

/// Strict mode: every object names every property in `required` and carries
/// `additionalProperties: false`; what is optional is nullable. The `known`
/// array contains only facts supplied in the question. Catalog growth must
/// not force hundreds of null fields into every response.
fn research_schema(catalogue: &crate::plan::reading::ReaderCatalogue) -> Value {
    let nullable_string = json!({ "type": ["string", "null"] });
    let kinds = catalogue.kinds();
    let capability_enum: Vec<&String> = catalogue.outputs_by_capability.keys().collect();
    // Fact names are validated against the live catalog by from_question.
    // Encoding hundreds as nested enums makes the provider reject the schema
    // for excessive branching before it reads even a one-line question.

    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["question", "resolved_entities", "execution_plan", "expected_output"],
        "properties": {
            "question": {
                "type": "object",
                "additionalProperties": false,
                "required": ["text", "text_en", "intent", "language", "recency_days", "clarifying_question"],
                "properties": {
                    "text": { "type": "string" },
                    "text_en": { "type": "string" },
                    "intent": { "type": "string" },
                    "language": { "type": "string" },
                    "recency_days": { "type": ["integer", "null"] },
                    "clarifying_question": { "type": ["string", "null"] }
                }
            },
            "resolved_entities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "name", "country", "known"],
                    "properties": {
                        "kind": { "type": "string", "enum": kinds },
                        "name": { "type": "string" },
                        "country": nullable_string,
                        "known": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["fact_type", "value"],
                                "properties": {
                                    "fact_type": { "type": "integer" },
                                    "value": { "type": "string" }
                                }
                            }
                        }
                    }
                }
            },
            "execution_plan": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["step", "capability", "subject", "outputs"],
                    "properties": {
                        "step": { "type": "integer" },
                        "capability": { "type": "string", "enum": capability_enum },
                        "subject": { "type": "string" },
                        "outputs": { "type": "array", "items": { "type": "integer" } }
                    }
                }
            },
            "expected_output": { "type": "array", "items": { "type": "string" } }
        }
    })
}

fn read_system_prompt(capabilities: &[String]) -> String {
    format!(
        "You convert a business research question into a strict execution object.\n\n\
         Rules:\n\
         - `question.text_en` is the question in English, translated as literally as possible. \
           Copy it verbatim when the question is already English. Proper nouns — company names, \
           domains, tickers — stay exactly as the asker wrote them.\n\
         - Resolve an entity to its legal name when you are sure of it, and add the country \
           (ISO-3166 alpha-2) and domain when you know them.\n\
         - `identifiers` holds ONLY values that appear in the question itself. Never supply a \
           registration number, KVK number, LEI, VAT number or ticker from memory, not even \
           for a well-known company: a number you recall becomes a paid lookup of the wrong \
           company. Leave it null and the asker will be asked for it.\n\
         - One execution step per distinct sub-question, numbered from 1 in logical order.\n\
         - `capability` must come from this list, exactly: {}\n\
         - `expected_output` names what the asker wants back, one empty object per kind.\n\
         - `question.language` is the ISO-639-1 code of the language the asker wrote in \
           (en, nl, de, ...).\n\
         - `question.recency_days` is the requested freshness window as a whole number of \
           days (today=1, last week=7, last month=30), or null when the question states none.\n\
         - `question.clarifying_question`: when the question is too vague to know which \
           kind of data source would answer it — no clear subject, no clear kind of data \
           wanted, or an ambiguous scope such as which country or register — write ONE \
           short question in the asker's language asking for exactly that. Null when the \
           question is specific enough to act on.",
        capabilities.join(", ")
    )
}

/// Het schema van het geparseerde object.
///
/// STRICT MODE HEEFT EEN HARDE VORM, en die kost twee dingen die je normaal
/// gratis krijgt. Elk object moet ELKE eigenschap in `required` noemen en
/// `additionalProperties: false` dragen — er bestaat geen optioneel veld. Wat
/// optioneel hoort te zijn, is dus een veld dat `null` mag zijn.
///
/// De eerste versie hiervan liet `required` weg op het geneste
/// `identifiers`-object en gebruikte `additionalProperties: true` voor vrije
/// invoer. OpenRouter weigerde de hele call met een 400, en omdat een mislukte
/// parse netjes terugvalt op de lexicale route zag je daar niets van behalve
/// een leeg antwoord. Zie de test `strict_mode_shape` hieronder: die controleert
/// deze vorm, zodat dezelfde fout niet opnieuw ongemerkt kan ontstaan.
fn read_schema(capabilities: &[String]) -> Value {
    let capability_enum: Vec<&str> = capabilities.iter().map(String::as_str).collect();

    // Een veld dat leeg mag blijven. In strict mode is dat een nullable type,
    // niet een weggelaten `required`.
    let nullable_string = json!({ "type": ["string", "null"] });

    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["question", "resolved_entities", "execution_plan", "expected_output"],
        "properties": {
            "question": {
                "type": "object",
                "additionalProperties": false,
                "required": ["text", "text_en", "intent", "language", "recency_days", "clarifying_question"],
                "properties": {
                    "text": { "type": "string" },
                    // DE VERTALING DRAAGT DE ZOEKOPDRACHT.
                    //
                    // De catalogus is Engels; de vraag hoeft dat niet te zijn.
                    // Zonder dit veld scoort "wie is de eigenaar van Mollie" op
                    // `eigenaar` — een woord dat in geen enkel pad voorkomt — en
                    // valt élke Nederlandse vraag door de ondergrens, terwijl
                    // FilingsPulse letterlijk `/api/filings/ownership` aanbiedt.
                    //
                    // Het model dat de vraag toch al leest, is precies wat dit
                    // kan: één veld erbij in dezelfde call, geen tweede model,
                    // geen extra latency.
                    "text_en": { "type": "string", "description": "The same question in English, translated as literally as possible. Copy it verbatim when it is already English. Keep proper nouns — company names, domains, tickers, identifiers — exactly as the asker wrote them." },
                    "intent": { "type": "string", "description": "Short label, e.g. company_research." },
                    // Voor de vraag terug. Het sjabloon in `ask::needs_context`
                    // moet weten in welke taal het moet vragen, en de lezer
                    // heeft de vraag toch al gelezen.
                    "language": { "type": "string", "description": "ISO-639-1 code of the language the asker wrote in, e.g. en, nl." },
                    // The same reading feeds form prefill. Keeping the window
                    // here prevents the App from interpreting the sentence a
                    // second time merely to fill a date or `days` field.
                    "recency_days": { "type": ["integer", "null"], "description": "Requested freshness window in whole days: today=1, last week=7, last month=30. Null when no window was stated." },
                    // De lezer ziet de catalogus niet, maar hij ziet wel of de
                    // vraag een onderwerp en een soort gegevens noemt. Een
                    // vraag die dat niet doet, kan geen bron kiezen — en dan is
                    // dit de vraag die het antwoord terugstelt als het
                    // herordeningsmodel er geen betere heeft.
                    "clarifying_question": { "type": ["string", "null"], "description": "One short question, in the asker's language, when the question is too vague to route to a kind of data source. Null when it is specific enough." }
                }
            },
            "resolved_entities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["type", "name", "country", "domain", "identifiers"],
                    "properties": {
                        "type": { "type": "string", "enum": ["company", "person", "domain", "asset", "place", "other"] },
                        "name": { "type": "string" },
                        "country": { "type": ["string", "null"], "description": "ISO-3166-1 alpha-2, or null when the question does not say." },
                        "domain": nullable_string,
                        "identifiers": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kvk_number", "company_number", "lei", "vat_number", "ticker"],
                            "properties": {
                                // Null unless the question states it. A guessed
                                // registration number becomes a paid call that
                                // looks up nothing.
                                "kvk_number": nullable_string,
                                "company_number": nullable_string,
                                "lei": nullable_string,
                                "vat_number": nullable_string,
                                "ticker": nullable_string
                            }
                        }
                    }
                }
            },
            "execution_plan": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["step", "capability", "subject"],
                    "properties": {
                        "step": { "type": "integer" },
                        "capability": { "type": "string", "enum": capability_enum },
                        // Was a free-form `input` object. Strict mode has no way
                        // to say "any keys", and the planner fills the inputs
                        // from the entities anyway — so this names WHICH entity
                        // the step is about, which is the only part the model
                        // knew that we did not.
                        "subject": { "type": "string", "description": "The entity name this step is about." }
                    }
                }
            },
            "expected_output": {
                "type": "array",
                "description": "What the asker wants back: ownership, financial_health, leadership, news, …",
                "items": { "type": "string" }
            }
        }
    })
}

/// Stap 3: de shortlist laten herordenen. `None` als er geen model is of het faalt.
async fn rerank(
    state: &Arc<AppState>,
    question: &str,
    parsed: Option<&Value>,
    candidates: &[ask::Candidate],
    limit: usize,
) -> Option<Value> {
    let vendor = crate::query_plan::ask_rerank_vendor(&state.config)?;

    let user = json!({
        "question": question,
        "parsed_query": parsed,
        "candidates": candidates,
        "max_endpoints": limit,
        // `match_floor` stond hier ooit, zodat het model zelf kon bepalen of
        // het een contextvraag moest schrijven. Dat maakte het medebeslisser
        // over een drempel die de gateway toch al zelf toepast, en bij het
        // kleinste meningsverschil bleef de steller met een leeg formulier
        // achter. Het model levert nu altijd de vraag; de lat blijft hier.
        // De lezer heeft de taal al bepaald; zonder dit veld schreef Haiku
        // bij een Nederlandse vraag een Engelse vraag terug.
        "asker_language": parsed
            .and_then(|p| p.pointer("/question/language"))
            .and_then(Value::as_str),
    })
    .to_string();

    // OPNIEUW PROBEREN, WANT DEZE CALL DRAAGT HET HELE OORDEEL.
    //
    // Mislukt hij, dan rangschikt niets de kandidaten meer: `match_score` valt
    // terug op woordtelling, en dat is een andere schaal met een ander bereik
    // (zie [`ask::MATCH_FLOOR`]). Het antwoord blijft dan bruikbaar, maar het
    // is meetbaar slechter geordend — reden genoeg om een tijdelijke storing
    // niet meteen te accepteren. Zie [`crate::config::DEFAULT_ASK_RERANK_ATTEMPTS`]
    // voor de ruil met de wachttijd.
    let timeout = Duration::from_millis(state.config.ask_rerank_timeout_ms);
    let attempts = state.config.ask_rerank_attempts.max(1);

    for attempt in 1..=attempts {
        match crate::query_plan::call_structured(
            state,
            &vendor,
            ask::RERANK_SYSTEM_PROMPT,
            user.clone(),
            ask::rerank_schema(limit),
            1_200,
            "ask rerank",
            timeout,
        )
        .await
        {
            Ok(raw) => {
                if attempt > 1 {
                    tracing::info!(attempt, attempts, "ask: rerank succeeded on a retry");
                }
                return Some(raw);
            }
            Err(err) => {
                let retryable = crate::query_plan::worth_retrying(&err);
                let last = attempt == attempts;
                tracing::warn!(
                    attempt,
                    attempts,
                    retryable,
                    "ask: rerank failed: {err}{}",
                    if retryable && !last {
                        "; retrying"
                    } else {
                        "; giving up, the answer falls back to lexical order"
                    }
                );
                if !retryable {
                    return None;
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_answer_names_the_engine_that_produced_it() {
        // The App reads this instead of a build-time `VITE_` flag, so both
        // transports and every exit from `run_ask` must carry it. A missing
        // field would silently read as "v1" downstream, which is exactly the
        // ambiguity a rollback cannot afford.
        let answer = || AskResponse {
            question: "q".into(),
            parsed: None,
            endpoints: Vec::new(),
            needs_input: None,
            needs_context: None,
            meta: serde_json::json!({ "found": false }),
        };

        let v1 = stamp_engine(answer(), "v1");
        assert_eq!(v1.meta["engine"], "v1");
        // The pre-existing keys survive the stamp.
        assert_eq!(v1.meta["found"], false);

        let v2 = stamp_engine(answer(), "v2");
        assert_eq!(v2.meta["engine"], "v2");
    }

    #[test]
    fn research_reading_is_sparse_even_with_thousands_of_catalog_inputs() {
        use crate::plan::reading::{InputFact, ReaderCatalogue};
        let catalogue = ReaderCatalogue {
            literal_url_inputs: std::collections::BTreeSet::new(),
            inputs_by_capability: std::collections::BTreeMap::new(),
            inputs: (0..2750)
                .map(|i| {
                    (
                        format!("weather.input{i}"),
                        InputFact {
                            kind: "weather".into(),
                            pattern: None,
                            value_type: "string".into(),
                        },
                    )
                })
                .collect(),
            outputs_by_capability: std::collections::BTreeMap::from([(
                "weather.lookup".into(),
                vec!["weather.temperature".into()],
            )]),
        };
        let schema = research_schema(&catalogue);
        let known = &schema["properties"]["resolved_entities"]["items"]["properties"]["known"];
        assert_eq!(known["type"], json!("array"));
        assert_eq!(known["items"]["required"], json!(["fact_type", "value"]));
        assert!(known["items"]["properties"]["fact_type"]
            .get("enum")
            .is_none());
        walk_objects(&schema, "root", &mut |obj, _| {
            assert_eq!(obj["additionalProperties"], json!(false));
            for key in obj["properties"].as_object().unwrap().keys() {
                assert!(obj["required"].as_array().unwrap().contains(&json!(key)));
            }
        });
    }

    fn offer() -> crate::db::CapabilityOffer {
        crate::db::CapabilityOffer {
            capability_slug: "company.lookup".into(),
            endpoint_id: None,
            api_slug: "working-provider".into(),
            api_name: "Working provider".into(),
            endpoint_method: "GET".into(),
            endpoint_path: "/company".into(),
            endpoint_description: Some("Company lookup".into()),
            price_usd: 0.01,
            payment_required: true,
            hosted_externally: false,
            call_url: Some("https://gateway.apiosk.com/working-provider/company".into()),
            input_map: json!({}),
            fixed_params: json!({}),
            request_body: json!({}),
            query_params: json!({}),
            notes: None,
            source: "reviewed".into(),
            confidence: 1.0,
            provider_name: Some("Working provider".into()),
            provider_username: Some("working-provider".into()),
            search_text: None,
        }
    }

    fn quarantined_stats() -> crate::db::ProviderStats {
        crate::db::ProviderStats {
            api_slug: "working-provider".into(),
            attempts: 20,
            successes: 4,
            recent_attempts: 10,
            recent_successes: 2,
            success_rate: 0.2,
            p50_ms: Some(100),
            p95_ms: Some(200),
        }
    }

    /// Wandelt door een schema en roept `check` aan voor elk object met
    /// eigenschappen, inclusief die in `items`.
    fn walk_objects(v: &Value, path: &str, check: &mut impl FnMut(&Value, &str)) {
        if v.get("properties").is_some() {
            check(v, path);
        }
        if let Some(props) = v.get("properties").and_then(|p| p.as_object()) {
            for (k, sub) in props {
                walk_objects(sub, &format!("{path}.{k}"), check);
            }
        }
        if let Some(items) = v.get("items") {
            walk_objects(items, &format!("{path}[]"), check);
        }
    }

    #[test]
    fn strict_mode_shape() {
        // WAT DIT TEGENHOUDT: OpenRouter weigerde de hele call met
        //   "/properties/resolved_entities/items/properties/identifiers/required:
        //    `required` is required to be supplied and to be an array including
        //    every key in properties"
        // omdat het geneste identifiers-object geen `required` had. De parse
        // viel netjes terug op de lexicale route, dus in het antwoord was er
        // niets van te zien behalve dat het parsed-object leeg bleef.
        //
        // Strict mode kent geen optioneel veld: elk object noemt ELKE
        // eigenschap in `required` en verbiedt extra velden. Optioneel is een
        // nullable type.
        let schema = read_schema(&["company_registry.lookup".to_string()]);
        let mut problems: Vec<String> = Vec::new();

        walk_objects(&schema, "root", &mut |obj, path| {
            if obj["additionalProperties"] != json!(false) {
                problems.push(format!("{path}: additionalProperties moet false zijn"));
            }
            let keys: Vec<&String> = obj["properties"]
                .as_object()
                .map(|m| m.keys().collect())
                .unwrap_or_default();
            let required: Vec<&str> = obj["required"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for key in keys {
                if !required.contains(&key.as_str()) {
                    problems.push(format!("{path}: '{key}' ontbreekt in required"));
                }
            }
        });

        assert!(
            problems.is_empty(),
            "strict mode geschonden:\n  {}",
            problems.join("\n  ")
        );
    }

    #[test]
    fn optional_fields_are_nullable_rather_than_absent() {
        // De prijs van strict mode: "mag leeg blijven" is een type, geen
        // weggelaten required. Een identifier die het model niet zeker weet,
        // moet null KUNNEN zijn — anders vult het er iets in.
        let schema = read_schema(&["a.b".to_string()]);
        let ids = &schema["properties"]["resolved_entities"]["items"]["properties"]["identifiers"];
        for field in [
            "kvk_number",
            "lei",
            "vat_number",
            "ticker",
            "company_number",
        ] {
            assert_eq!(
                ids["properties"][field]["type"],
                json!(["string", "null"]),
                "{field} moet null kunnen zijn"
            );
        }
    }

    #[test]
    fn the_read_schema_offers_only_real_capabilities() {
        // A capability the catalogue cannot serve is a plan step nothing can
        // execute, so the model must not be able to name one.
        let caps = vec![
            "company_registry.lookup".to_string(),
            "people.search".to_string(),
        ];
        let schema = read_schema(&caps);
        let listed = schema["properties"]["execution_plan"]["items"]["properties"]["capability"]
            ["enum"]
            .as_array()
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|v| v == "company_registry.lookup"));
    }

    #[test]
    fn the_read_schema_forbids_extra_fields() {
        let schema = read_schema(&["a.b".to_string()]);
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(
            schema["properties"]["question"]["additionalProperties"],
            json!(false)
        );
        // `expected_output` was een object met vrije sleutels. Strict mode kan
        // "any keys" niet uitdrukken, dus het is een lijst met namen geworden —
        // wat het ook eerlijker maakt: het zijn namen, geen lege objecten.
        assert_eq!(
            schema["properties"]["expected_output"]["type"],
            json!("array")
        );
        assert_eq!(
            schema["properties"]["expected_output"]["items"]["type"],
            json!("string")
        );
    }

    #[test]
    fn the_prompt_carries_the_capability_list() {
        let p = read_system_prompt(&["company_registry.lookup".to_string()]);
        assert!(p.contains("company_registry.lookup"));
    }

    #[test]
    fn the_read_schema_asks_for_language_and_a_clarifying_question() {
        // Zonder taalcode vraagt het sjabloon in de verkeerde taal; zonder
        // verhelderende vraag heeft het antwoord bij een lege shortlist alleen
        // het sjabloon. Allebei in `required`, want strict mode.
        let schema = read_schema(&["a.b".to_string()]);
        let q = &schema["properties"]["question"];
        let required: Vec<&str> = q["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(required.contains(&"language"));
        assert!(required.contains(&"recency_days"));
        assert!(required.contains(&"clarifying_question"));
        assert_eq!(q["properties"]["language"]["type"], json!("string"));
        assert_eq!(
            q["properties"]["recency_days"]["type"],
            json!(["integer", "null"])
        );
        assert_eq!(
            q["properties"]["clarifying_question"]["type"],
            json!(["string", "null"])
        );
    }

    #[test]
    fn the_match_floor_is_the_default_unless_asked_and_then_clamped() {
        assert_eq!(match_floor(None), ask::MATCH_FLOOR);
        assert_eq!(match_floor(Some(60)), 60);
        assert_eq!(match_floor(Some(-5)), 0);
        assert_eq!(match_floor(Some(120)), 100);
    }

    #[test]
    fn limit_is_clamped_to_something_sane() {
        // A caller asking for 500 endpoints gets 10, not a bill for 500.
        for (asked, want) in [
            (None, ask::TOP_N),
            (Some(3), 3),
            (Some(0), 1),
            (Some(500), 10),
        ] {
            let got = asked.unwrap_or(ask::TOP_N).clamp(1, 10);
            assert_eq!(got, want, "limit={asked:?}");
        }
    }

    #[test]
    fn canonical_ask_excludes_quarantined_and_uncallable_offers() {
        let mut candidate = offer();
        assert!(eligible_for_ask(&candidate, None, None));
        assert!(!eligible_for_ask(
            &candidate,
            Some(&quarantined_stats()),
            None
        ));

        candidate.call_url = None;
        assert!(!eligible_for_ask(&candidate, None, None));
    }

    #[test]
    fn canonical_ask_applies_the_buyer_total_price_before_selection() {
        let candidate = offer();
        let buyer_total = super::super::flow::buyer_price_usd(&candidate);
        assert!(eligible_for_ask(&candidate, None, Some(buyer_total)));
        assert!(!eligible_for_ask(
            &candidate,
            None,
            Some(buyer_total - 0.000_001)
        ));
    }
}
