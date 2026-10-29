//! Measured latency and reliability per listing, from calls that reached the
//! provider.
//!
//! The request log holds 2.4M rows, but 1.96M of them are `402` challenges:
//! the gateway telling a caller to pay, without touching the origin. Their
//! latency is Apiosk's paywall, not the provider's service, and it clusters
//! around 440ms for everyone. Publishing that as "latency" would say every
//! provider is equally fast, which is both false and flattering to the slow
//! ones. So the denominator is 2xx plus 5xx: calls where the upstream was
//! reached and either answered or broke. A 402 is the payment protocol
//! working, and a caller 4xx is a bad path, not the provider's to answer for.
//!
//! ---------------------------------------------------------------------------
//! Why the sample floor counts attempts, not successes
//! ---------------------------------------------------------------------------
//!
//! This query used to end `FROM ok ... HAVING COUNT(*) >= 30` — thirty
//! *successful* calls before a listing was reported at all. That floor is
//! right for a percentile and exactly wrong for a success rate, because it
//! silences the listings that need reporting most: one that fails every call
//! has zero successes, never reaches thirty, and comes back absent rather
//! than terrible. Absent is then read downstream as "unmeasured", and
//! `v1_routes/flow.rs::score` deliberately does not penalise unmeasured — so
//! the worse a listing got, the more certainly it escaped its own record.
//!
//! Two floors, because the two numbers need different amounts of evidence:
//!
//!   * **[`MIN_ATTEMPTS`] tried calls** to report a success rate. Ten calls
//!     that all failed is a finding; two is a bad afternoon.
//!   * **[`MIN_LATENCY_SAMPLE`] successful calls** to report percentiles,
//!     which is the old floor kept where it belongs. Below it `p50_ms` and
//!     `p95_ms` are `None` — absent, never zero, because an agent that reads
//!     `p50_ms: 0` concludes "instant". A listing can therefore be measured
//!     for reliability and unmeasured for speed, which is the honest shape:
//!     you cannot time calls that never succeeded.
//!
//! Sample size traveling with every number is the other half of that. A p95
//! over 40 calls is not a p95, so `attempts` and `successes` are published
//! alongside, and a reader can discount them.
//!
//! ---------------------------------------------------------------------------
//! A record is not a condition
//! ---------------------------------------------------------------------------
//!
//! Ninety days is the right window to describe a provider and the wrong one to
//! judge it by. Measured over ninety days positionstack serves 11% of its
//! calls — and every one of those failures landed on a single day, 2026-07-02,
//! with nothing but successes since. Holding it back today would punish a
//! listing that works, and it would take months for the old mass to wash out.
//! The same reading cleared scrapestack, marketstack and two others: of the
//! ten listings the ninety-day number condemned, nine were fine.
//!
//! So the two questions are asked over two windows. `success_rate` is the
//! record, over [`WINDOW_DAYS`], and it is what gets published. The gate runs
//! on [`RECENT_WINDOW_DAYS`] — what this listing is doing *now* — and so does
//! scoring, wherever there is recent evidence to score on.
//!
//! That also settles how a held-back listing gets out. It does not need an
//! amnesty or a probe: the recent window empties on its own, so a listing with
//! no traffic for two weeks is un-judged and back in the field automatically,
//! and one that starts working again is out from under it within days.
//!
//! Federated listings are absent rather than zero: Apiosk never proxies one,
//! so it has no timing for it at all.

use super::*;
use std::collections::HashMap;

/// Latency and reliability for one listing, over the measurement window.
#[derive(Debug, Clone, FromRow)]
pub struct ProviderStats {
    pub api_slug: String,
    /// Calls that reached the provider: served (2xx) or broken (5xx). The
    /// denominator for `success_rate`.
    pub attempts: i64,
    /// The 2xx subset — the numerator, and the sample the percentiles rest on.
    pub successes: i64,
    pub success_rate: f64,
    /// The same two counts over [`RECENT_WINDOW_DAYS`]: not the record, but
    /// the condition. Zero attempts means nobody has called it lately, which
    /// is an absence of evidence and never held against it.
    pub recent_attempts: i64,
    pub recent_successes: i64,
    /// `None` below [`MIN_LATENCY_SAMPLE`] successes. Absent, not zero.
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
}

/// Tried calls before a success rate is reported at all.
const MIN_ATTEMPTS: i64 = 10;

/// Successful calls before percentiles are reported. Below this, they are
/// noise dressed as measurement.
const MIN_LATENCY_SAMPLE: i64 = 30;

/// A record bad enough to act on: a listing that fails more often than it
/// works is not a candidate to hand somebody, it is an outage with a price.
pub const QUARANTINE_FLOOR: f64 = 0.5;

/// How recent "now" is. Short enough that a fixed listing is back within days
/// and an old outage stops counting, long enough to hold a sample at these
/// volumes.
pub const RECENT_WINDOW_DAYS: i64 = 14;

/// Recent calls before the gate will act. Deliberately low, and lower than
/// [`MIN_ATTEMPTS`], because the two errors are not equally bad: holding back
/// a working listing costs it some default traffic for at most two weeks and
/// it stays callable by name, while offering a broken one costs somebody the
/// answer they came for. Five failures inside a fortnight is enough to stop
/// recommending something.
pub const QUARANTINE_MIN_ATTEMPTS: i64 = 5;

/// How far back to measure. Long enough to gather a sample at current volumes,
/// short enough that a provider who fixed their latency last month is not
/// judged on the month before.
const WINDOW_DAYS: i64 = 90;

impl Database {
    /// Measured stats per listing slug, for every listing with a usable sample.
    ///
    /// Returns a map so a caller can look up the listings it is about to
    /// publish without a query per candidate.
    pub async fn provider_latency_stats(&self) -> Result<HashMap<String, ProviderStats>> {
        let query = Self::latency_sql();

        // A connection of its own, with the pool's 5s ceiling lifted for this
        // statement only.
        //
        // WHY: the percentile scan grew with the log table — 3M rows and 2.4GB
        // at the time of writing — and now takes ~30s. Against the pool default
        // it could only ever time out, and because a failed cache fill is not
        // remembered, EVERY discovery request paid the full 5s to fail again.
        // Five of seven seconds, on every query, for a number that was always
        // empty afterwards. Worse, the emptiness was silent: ranking simply ran
        // without measured latency and nothing said so.
        //
        // `SET LOCAL` scopes the raise to this transaction, so no other query
        // inherits it. Only the background refresher calls this; the request
        // path reads the cache and never waits (`Cache::peek_provider_stats`).
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout = '120000ms'")
            .execute(&mut *tx)
            .await?;

        let rows = sqlx::query_as::<_, ProviderStats>(&query)
            .fetch_all(&mut *tx)
            .await?;

        tx.commit().await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.api_slug.clone(), row))
            .collect())
    }

    fn latency_sql() -> String {
        format!(
            r#"WITH scoped AS (
                 SELECT l.api_slug, l.status_code, l.latency_ms, l.occurred_at
                 FROM gateway_request_logs l
                 -- Only slugs that are a real listing. The log stamps api_slug
                 -- on anything that looks like one, so without this join the
                 -- results include logo.svg, robots, sitemap.xml and the
                 -- gateway's own /mcp and /f surfaces as though they were
                 -- providers.
                 JOIN apis a ON a.slug = l.api_slug
                 WHERE l.api_slug IS NOT NULL
                   AND l.latency_ms IS NOT NULL
                   AND l.occurred_at > now() - interval '{WINDOW_DAYS} days'
                   -- Reached and answered, or reached and broke. Everything
                   -- else is not the provider's to answer for: counting caller
                   -- 4xx made serper look like it succeeds 1.5% of the time,
                   -- which says more about agents probing wrong URLs than
                   -- about serper.
                   AND ((l.status_code >= 200 AND l.status_code < 300)
                         OR l.status_code >= 500)
               ),
               agg AS (
                 SELECT
                   api_slug,
                   COUNT(*)::BIGINT AS attempts,
                   COUNT(*) FILTER (
                     WHERE status_code >= 200 AND status_code < 300
                   )::BIGINT AS successes,
                   -- The same pair over the recent window. This is what the
                   -- gate reads: a listing is held back for what it is doing,
                   -- not for what it did in June.
                   COUNT(*) FILTER (
                     WHERE occurred_at > now() - interval '{RECENT_WINDOW_DAYS} days'
                   )::BIGINT AS recent_attempts,
                   COUNT(*) FILTER (
                     WHERE occurred_at > now() - interval '{RECENT_WINDOW_DAYS} days'
                       AND status_code >= 200 AND status_code < 300
                   )::BIGINT AS recent_successes,
                   -- Percentiles over the served calls only. A 3xx is a
                   -- redirect, not a served call, and lands in single-digit
                   -- milliseconds: including redirects put positionstack at a
                   -- median of 0ms, which is not a latency but the absence of
                   -- one.
                   percentile_cont(0.5) WITHIN GROUP (ORDER BY latency_ms)
                     FILTER (WHERE status_code >= 200 AND status_code < 300
                               AND latency_ms > 0) AS p50,
                   percentile_cont(0.95) WITHIN GROUP (ORDER BY latency_ms)
                     FILTER (WHERE status_code >= 200 AND status_code < 300
                               AND latency_ms > 0) AS p95
                 FROM scoped
                 GROUP BY api_slug
                 HAVING COUNT(*) >= {MIN_ATTEMPTS}
               )
               SELECT
                 api_slug,
                 attempts,
                 successes,
                 recent_attempts,
                 recent_successes,
                 (successes::double precision / NULLIF(attempts, 0)) AS success_rate,
                 -- Absent, not zero, below the percentile floor: a listing can
                 -- be measured for reliability and unmeasured for speed.
                 CASE WHEN successes >= {MIN_LATENCY_SAMPLE}
                      THEN p50::BIGINT END AS p50_ms,
                 CASE WHEN successes >= {MIN_LATENCY_SAMPLE}
                      THEN p95::BIGINT END AS p95_ms
               FROM agg"#
        )
    }

    #[cfg(test)]
    fn latency_sql_for_test() -> String {
        Self::latency_sql()
    }
}

impl ProviderStats {
    /// How often it has answered lately, or `None` if nobody has called it
    /// inside the recent window.
    pub fn recent_success_rate(&self) -> Option<f64> {
        (self.recent_attempts > 0)
            .then(|| self.recent_successes as f64 / self.recent_attempts as f64)
    }

    /// The success rate a decision should be made on: recent where there is
    /// recent evidence, the long record where there is not.
    ///
    /// Ranking uses this rather than [`Self::success_rate`] for the same
    /// reason the gate does. positionstack's ninety-day 11% is one bad day in
    /// July; scoring it as an 11% provider today is a wrong statement about
    /// the present.
    pub fn current_success_rate(&self) -> f64 {
        match self.recent_success_rate() {
            Some(rate) if self.recent_attempts >= QUARANTINE_MIN_ATTEMPTS => rate,
            _ => self.success_rate,
        }
    }

    /// Whether this listing has earned removal from the default field.
    ///
    /// Two conditions, both required and both about the recent window: a rate
    /// worse than a coin flip, and enough recent calls that the rate means
    /// something. A listing that meets both is still callable by name and
    /// still offered to a caller who asks for it explicitly — the gate is on
    /// what Apiosk *hands* somebody, not on what it permits.
    ///
    /// Nothing has to lift it. The window slides, so a listing that starts
    /// working climbs out within days, and one nobody calls for a fortnight
    /// has no recent record to be held against and is back in the field on
    /// its own. There is no starvation state to design around, and no amnesty
    /// anyone has to remember to run.
    pub fn is_quarantined(&self) -> bool {
        self.recent_attempts >= QUARANTINE_MIN_ATTEMPTS
            && self
                .recent_success_rate()
                .is_some_and(|r| r < QUARANTINE_FLOOR)
    }

    /// The stats block published next to an offer.
    ///
    /// Carries its own sample size and window so the numbers can be discounted
    /// by whoever reads them.
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "success_rate": (self.success_rate * 1000.0).round() / 1000.0,
            "measured_calls": self.attempts,
            "successful_calls": self.successes,
            "window_days": WINDOW_DAYS,
            // The record and the condition, side by side. A listing at 11%
            // over ninety days and 100% over the last fortnight is not
            // described by either number alone.
            "recent": {
                "window_days": RECENT_WINDOW_DAYS,
                "measured_calls": self.recent_attempts,
                "success_rate": self.recent_success_rate()
                    .map(|r| (r * 1000.0).round() / 1000.0),
            },
            "basis": "Calls proxied by Apiosk that reached the provider — served (2xx) or broken (5xx). Payment challenges are excluded: their timing is the gateway's, not the provider's.",
        });
        // Written only when they exist. A null p95 next to a real success rate
        // says "not timed yet"; a zero would say "instant".
        if let (Some(p50), Some(p95)) = (self.p50_ms, self.p95_ms) {
            value["p50_ms"] = serde_json::json!(p50);
            value["p95_ms"] = serde_json::json!(p95);
        }
        if self.is_quarantined() {
            value["quarantined"] = serde_json::json!(true);
            value["quarantine_reason"] = serde_json::json!(format!(
                "Failing now: {:.0}% of its last {} calls, over the past {RECENT_WINDOW_DAYS} days, did not come back. Apiosk stops offering a listing by default below {:.0}% success; it stays callable by name, and returns to the field on its own once it works again.",
                (1.0 - self.recent_success_rate().unwrap_or_default()) * 100.0,
                self.recent_attempts,
                QUARANTINE_FLOOR * 100.0,
            ));
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing whose recent record matches its long one.
    fn stats(attempts: i64, successes: i64) -> ProviderStats {
        ProviderStats {
            api_slug: "x".into(),
            attempts,
            successes,
            recent_attempts: attempts,
            recent_successes: successes,
            success_rate: successes as f64 / attempts as f64,
            p50_ms: (successes >= MIN_LATENCY_SAMPLE).then_some(198),
            p95_ms: (successes >= MIN_LATENCY_SAMPLE).then_some(443),
        }
    }

    #[test]
    fn payment_challenges_never_count_as_provider_latency() {
        // 1.96M of 2.4M logged requests are 402s at a flat ~440ms. Including
        // them would report every provider as equally fast.
        let sql = Database::latency_sql_for_test();
        assert!(sql.contains("status_code >= 200 AND status_code < 300"));
        assert!(sql.contains("latency_ms > 0"));
        assert!(sql.contains("l.status_code >= 500"));
        assert!(!sql.contains("s.status_code <> 402"));
    }

    #[test]
    fn only_real_listings_are_measured() {
        let sql = Database::latency_sql_for_test();
        // Excluded structurally, by joining to real listings, rather than by
        // naming every asset route the log might stamp a slug onto.
        assert!(sql.contains("JOIN apis a ON a.slug = l.api_slug"));
    }

    #[test]
    fn the_sample_floor_counts_attempts_so_a_total_failure_is_reported() {
        let sql = Database::latency_sql_for_test();
        // The regression this whole module exists to prevent: a HAVING over
        // the successful subset hides the listings that never succeed.
        assert!(sql.contains(&format!("HAVING COUNT(*) >= {MIN_ATTEMPTS}")));
        assert!(
            sql.contains("FROM scoped\n                 GROUP BY api_slug"),
            "the group must be over every attempt, not over the 2xx subset"
        );
        // And percentiles keep the stricter floor, gated separately.
        assert!(sql.contains(&format!("successes >= {MIN_LATENCY_SAMPLE}")));
        // And the gate's own window has to reach the query too, or the
        // verdict silently falls back to the ninety-day record.
        assert!(sql.contains(&format!("interval '{RECENT_WINDOW_DAYS} days'")));
    }

    #[test]
    fn a_listing_failing_now_is_quarantined() {
        // shipment-api, as measured 2026-08-30: 1 served of its last 63 calls,
        // still failing today.
        let mut s = stats(374, 71);
        s.recent_attempts = 63;
        s.recent_successes = 1;
        assert!(s.is_quarantined());
        let j = s.to_json();
        assert_eq!(j["quarantined"], true);
        assert!(j["quarantine_reason"]
            .as_str()
            .unwrap()
            .contains("callable by name"));
    }

    /// The failure that rewrote this module. Judged on ninety days, nine of
    /// the ten listings the gate first caught were working fine.
    #[test]
    fn an_old_outage_does_not_sentence_a_listing_that_works_now() {
        // positionstack: 35 served of 320 tried over ninety days — and every
        // failure on one day, 2026-07-02, with successes ever since.
        let mut s = stats(320, 35);
        s.recent_attempts = 12;
        s.recent_successes = 12;

        assert!(
            !s.is_quarantined(),
            "it has worked every time for a fortnight"
        );
        // And it must not be *scored* as an 11% provider either.
        assert_eq!(s.current_success_rate(), 1.0);
        assert_eq!(
            s.success_rate,
            35.0 / 320.0,
            "the record itself is unchanged"
        );

        // Both numbers are published, because neither describes it alone.
        let j = s.to_json();
        assert_eq!(j["success_rate"], 0.109);
        assert_eq!(j["recent"]["success_rate"], 1.0);
        assert_eq!(j["recent"]["measured_calls"], 12);
    }

    /// A listing nobody has called lately has no current record. Absence of
    /// evidence is not evidence, in either direction.
    #[test]
    fn silence_is_never_held_against_a_listing() {
        // aviationstack: 16% over ninety days, no traffic at all in the last
        // fortnight. This is also how a held-back listing gets out — the
        // window empties and nothing has to lift it.
        let mut s = stats(1756, 283);
        s.recent_attempts = 0;
        s.recent_successes = 0;

        assert!(!s.is_quarantined());
        assert_eq!(s.recent_success_rate(), None);
        // With nothing recent to go on, the long record is what scoring has.
        assert_eq!(s.current_success_rate(), s.success_rate);
        assert_eq!(
            s.to_json()["recent"]["success_rate"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn a_bad_afternoon_is_not_a_quarantine() {
        // Two failures out of three is a worse rate than shipment-api's, and
        // must not remove a listing: the floor is evidence as well as ratio.
        let mut s = stats(3, 1);
        s.recent_attempts = 3;
        s.recent_successes = 1;
        assert!(!s.is_quarantined());
        assert!(s.to_json().get("quarantined").is_none());
    }

    #[test]
    fn reliability_is_reported_before_speed_can_be() {
        // 12 tried, 4 served: enough to state a success rate, nowhere near
        // enough to state a p95. The old floor returned nothing at all here.
        let s = stats(12, 4);
        let j = s.to_json();
        assert_eq!(j["success_rate"], 0.333);
        assert_eq!(j["measured_calls"], 12);
        assert!(
            j.get("p50_ms").is_none(),
            "a zero here reads as instant; absence reads as untimed"
        );
    }

    #[test]
    fn published_stats_carry_their_own_caveats() {
        let s = stats(253, 250);
        let j = s.to_json();
        assert_eq!(j["p50_ms"], 198);
        assert_eq!(j["measured_calls"], 253);
        assert_eq!(j["window_days"], WINDOW_DAYS);
        // Rounded, so a float artefact never ships as false precision.
        assert_eq!(j["success_rate"], 0.988);
        assert!(j["basis"]
            .as_str()
            .unwrap()
            .contains("Payment challenges are excluded"));
    }
}
