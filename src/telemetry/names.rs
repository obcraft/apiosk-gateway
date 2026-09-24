//! The closed vocabulary of telemetry lines. Every name here must equal an
//! entry in `perf/phases.json`; a unit test checks both directions.

macro_rules! names {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }
    };
}

names!(
    /// A timed server phase. `gw.db` is aggregated per request, not logged per call.
    Phase {
        Request => "gw.request",
        Auth => "gw.auth",
        Workspace => "gw.workspace",
        Claim => "gw.claim",
        Db => "gw.db",
        Catalog => "gw.catalog",
        Model => "gw.model",
        Plan => "gw.plan",
        View => "gw.view",
        Approve => "gw.approve",
        WorkerQueue => "gw.worker_queue",
        Worker => "gw.worker",
        DispatchReserve => "gw.dispatch_reserve",
        EdgeHop => "gw.edge_hop",
        CooldownWait => "gw.cooldown_wait",
        Analysis => "gw.analysis",
        DirectAnswer => "gw.direct_answer",
        SseStream => "gw.sse_stream",
    }
);

names!(
    /// A point on the question's journey, joined to its task and quote.
    Milestone {
        PriceReady => "price_ready",
        NeedsInput => "needs_input",
        Approved => "approved",
        WorkerStarted => "worker_started",
        FirstData => "first_data",
        SourcesReady => "sources_ready",
        AnswerComplete => "answer_complete",
        Terminal => "terminal",
    }
);

names!(
    /// Why a phase failed. Telemetry never carries an error message.
    ErrorClass {
        Unauthorized => "unauthorized",
        Forbidden => "forbidden",
        NotFound => "not_found",
        Busy => "busy",
        Timeout => "timeout",
        Upstream4xx => "upstream_4xx",
        Upstream5xx => "upstream_5xx",
        Transport => "transport",
        Validation => "validation",
        Db => "db",
        Conflict => "conflict",
        Unsupported => "unsupported",
        NeedsInput => "needs_input",
        QuoteChanged => "quote_changed",
        Budget => "budget",
        Cancelled => "cancelled",
        Internal => "internal",
        Dropped => "dropped",
    }
);

names!(
    /// Who asked: the App, a connected chatbot or an organisation key.
    Surface {
        Unknown => "unknown",
        App => "app",
        ConnectedAgent => "connected_agent",
        PublicApi => "public_api",
    }
);

names!(
    /// The unit of work a cost line or persisted cost row describes.
    Segment {
        Question => "question",
        Execution => "execution",
        RetryAnalysis => "retry_analysis",
        FollowUp => "follow_up",
    }
);

names!(
    /// How a cache answered for this trace.
    Cache {
        Hit => "hit",
        Miss => "miss",
        RefreshWait => "refresh_wait",
        StaleServed => "stale_served",
        FirstLoad => "first_load",
    }
);

names!(
    /// The settled state of one paid source call.
    SourceStatus {
        Succeeded => "succeeded",
        Failed => "failed",
        Unresolved => "unresolved",
        RefusedBeforeReservation => "refused_before_reservation",
        Refunded => "refunded",
    }
);

names!(
    /// The performance class a question is budgeted under.
    QueryClass {
        SimpleLookup => "simple_lookup",
        SearchList => "search_list",
        Comparison => "comparison",
        Dossier => "dossier",
        Unclassified => "unclassified",
    }
);

impl ErrorClass {
    /// The class of an HTTP status the gateway returned itself.
    pub fn from_status(status: u16) -> Option<Self> {
        Some(match status {
            0..=399 => return None,
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 | 410 => Self::NotFound,
            408 => Self::Timeout,
            409 => Self::Conflict,
            429 => Self::Busy,
            400..=499 => Self::Validation,
            502..=504 => Self::Upstream5xx,
            _ => Self::Internal,
        })
    }
    /// The class of an upstream (model or provider) HTTP status.
    pub fn upstream(status: u16) -> Self {
        match status {
            408 => Self::Timeout,
            429 => Self::Busy,
            400..=499 => Self::Upstream4xx,
            _ => Self::Upstream5xx,
        }
    }
}

/// A model, provider or source slug, or `other`. Free text never passes.
pub fn label(value: &str) -> &str {
    let valid = (1..=64).contains(&value.len())
        && value.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'/' | b':' | b'-'));
    if valid { value } else { "other" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn listed(key: &str) -> BTreeSet<String> {
        let phases: serde_json::Value = serde_json::from_str(include_str!("../../perf/phases.json")).unwrap();
        let entries = &phases[key];
        let names: Vec<String> = match entries {
            serde_json::Value::Array(items) => items.iter().map(|item| item.as_str().unwrap().to_owned()).collect(),
            serde_json::Value::Object(map) => map.keys().cloned().collect(),
            _ => panic!("missing {key} in perf/phases.json"),
        };
        let set: BTreeSet<_> = names.iter().cloned().collect();
        assert_eq!(set.len(), names.len(), "duplicate names in {key}");
        set
    }
    fn names<T: Copy>(all: &[T], text: impl Fn(T) -> &'static str) -> BTreeSet<String> {
        all.iter().map(|item| text(*item).to_owned()).collect()
    }

    #[test]
    fn every_name_matches_the_phase_dictionary_in_both_directions() {
        assert_eq!(names(Phase::ALL, Phase::as_str), listed("server_phases"));
        assert_eq!(names(Milestone::ALL, Milestone::as_str), listed("milestones"));
        assert_eq!(names(ErrorClass::ALL, ErrorClass::as_str), listed("error_classes"));
        assert_eq!(names(Surface::ALL, Surface::as_str), listed("surfaces"));
        assert_eq!(names(Segment::ALL, Segment::as_str), listed("segments"));
        assert_eq!(names(Cache::ALL, Cache::as_str), listed("cache_states"));
        assert_eq!(names(SourceStatus::ALL, SourceStatus::as_str), listed("source_statuses"));
        assert_eq!(names(QueryClass::ALL, QueryClass::as_str), listed("query_classes"));
    }

    #[test]
    fn labels_admit_only_short_slugs() {
        assert_eq!(label("google/gemini-3.8-flash"), "google/gemini-3.8-flash");
        assert_eq!(label("kvk:registry-profile"), "kvk:registry-profile");
        for text in ["", "Coolblue B.V.", "a b", "UPPER", "eyJhbGciOi", &"x".repeat(65)] {
            assert_eq!(label(text), "other", "{text}");
        }
    }

    #[test]
    fn http_statuses_map_to_closed_error_classes() {
        assert_eq!(ErrorClass::from_status(200), None);
        assert_eq!(ErrorClass::from_status(404), Some(ErrorClass::NotFound));
        assert_eq!(ErrorClass::from_status(422), Some(ErrorClass::Validation));
        assert_eq!(ErrorClass::from_status(503), Some(ErrorClass::Upstream5xx));
        assert_eq!(ErrorClass::upstream(429), ErrorClass::Busy);
        assert_eq!(ErrorClass::upstream(400), ErrorClass::Upstream4xx);
    }
}
