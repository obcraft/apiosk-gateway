use crate::{decode, Error, RULESET};
use serde::Deserialize;
use serde_json::{json, Value};

const MICRO: i64 = 1_000_000;
const MANUALS: [&str; 4] = [
    "https://download.belastingdienst.nl/toeslagen/docs/berekening_zorgtoeslag_tg0821z62fd.pdf",
    "https://download.belastingdienst.nl/toeslagen/docs/berekening_huurtoeslag_tg0831z62fd.pdf",
    "https://download.belastingdienst.nl/toeslagen/docs/berekening_kindgebonden_budget_tg0811z61fd.pdf",
    "https://download.belastingdienst.nl/toeslagen/docs/berekening_kinderopvangtoeslag_tg0801z61fd.pdf",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    year: u16,
    country: String,
    /// Whole-euro annual qualifying income, combined with partner where relevant.
    /// Huurtoeslag: rekeninkomen of all residents after statutory exemptions.
    annual_income_eur: i64,
    has_partner: bool,
    eligibility_confirmed: bool,
    stable_full_year: bool,
}
impl Context {
    fn validate(&self) -> Result<Option<&'static str>, Error> {
        if !(-99_999_999..=99_999_999).contains(&self.annual_income_eur) {
            return Err(Error::input(
                "annual_income_eur must be whole euros between -99999999 and 99999999",
            ));
        }
        Ok(if self.year != 2026 {
            Some("unsupported_year")
        } else if self.country != "NL" {
            Some("foreign_situation_requires_review")
        } else if !self.stable_full_year {
            Some("changes_during_year_require_review")
        } else if !self.eligibility_confirmed {
            Some("eligibility_conditions_not_confirmed")
        } else {
            None
        })
    }
    fn income(&self) -> i64 {
        self.annual_income_eur.max(0)
    }
}

fn cents(value: f64, field: &str) -> Result<i64, Error> {
    let scaled = value * 100.0;
    if !scaled.is_finite()
        || !(0.0..=9_999_999_900.0).contains(&scaled)
        || (scaled - scaled.round()).abs() > 0.00001
    {
        return Err(Error::input(format!(
            "{field} must be nonnegative euros with at most two decimals"
        )));
    }
    Ok(scaled.round() as i64)
}
fn display(micros: i64) -> f64 {
    (micros as f64 / 10_000.0).round() / 100.0
}
fn envelope(
    kind: &str,
    c: &Context,
    source: usize,
    monthly: Option<i64>,
    reason: Option<&str>,
    steps: Value,
) -> Value {
    let monthly = monthly.map(|v| v.max(0));
    json!({
        "data": {"benefit":kind,"year":c.year,"ruleset":RULESET,"currency":"EUR",
            "status":if monthly.is_some() {"estimate"} else {"requires_review"},
            "monthly_amount_eur":monthly.map(|m| m / MICRO),
            "monthly_before_rounding_eur":monthly.map(display),
            "annual_formula_amount_eur":monthly.map(|m| display(m * 12)),
            "reason":reason,"calculation":steps},
        "coverage":{"complete":false,"eligibility_assessed":false,
            "scope":"2026; Netherlands; unchanged relevant household and costs for the full year; eligibility conditions confirmed by caller",
            "unsupported":["other years","foreign situations","changes during year","special statutory exceptions","application or entitlement decision"]},
        "sources":[{"publisher":"Dienst Toeslagen","url":MANUALS[source],"year":2026,"verified_at":"2026-10-13"}],
        "notice":"Indicatieve berekening volgens de rekenregels van 2026, geen beschikking. Recht op toeslag is niet vastgesteld. Maandbedrag naar beneden afgerond volgens de rekenhandleiding; definitieve toekenning en betaaltermijnen kunnen afwijken."
    })
}
fn review(kind: &str, c: &Context, source: usize) -> Result<Option<Value>, Error> {
    Ok(c.validate()?
        .map(|why| envelope(kind, c, source, None, Some(why), json!({}))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Zorg {
    context: Context,
    assets_january1_eur: f64,
    applicant_age: u8,
    applicant_dutch_health_insurance: bool,
    partner_dutch_health_insurance: bool,
}
fn zorg(r: Zorg) -> Result<Value, Error> {
    let c = &r.context;
    let assets = cents(r.assets_january1_eur, "assets_january1_eur")?;
    if r.applicant_age > 130 {
        return Err(Error::input("applicant_age must be between 0 and 130"));
    }
    if !c.has_partner && r.partner_dutch_health_insurance {
        return Err(Error::input(
            "partner_dutch_health_insurance must be false without a partner",
        ));
    }
    if let Some(v) = review("zorgtoeslag", c, 0)? {
        return Ok(v);
    }
    let limit = if c.has_partner { 184_633 } else { 146_011 };
    let norm = 29_736 * if c.has_partner { 42_890 } else { 19_120 }
        + (c.income() - 29_736).max(0) * 137_300;
    let max_income = if c.has_partner { 51_142 } else { 40_857 };
    let standard = if c.has_partner { 4_238 } else { 2_119 } * MICRO;
    let reason = if r.applicant_age < 18 {
        Some("applicant_under_18")
    } else if !r.applicant_dutch_health_insurance {
        Some("applicant_not_dutch_insured")
    } else if assets > limit * 100 {
        Some("assets_above_limit")
    } else if c.income() > max_income {
        Some("income_above_limit")
    } else {
        None
    };
    let annual = if reason.is_some() {
        0
    } else {
        (standard - norm).max(0)
            / if c.has_partner && !r.partner_dutch_health_insurance {
                2
            } else {
                1
            }
    };
    Ok(envelope(
        "zorgtoeslag",
        c,
        0,
        Some(annual / 12),
        reason,
        json!({"standard_premium_eur":display(standard),"norm_premium_eur":display(norm),"asset_limit_eur":limit}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    context: Context,
    assets_january1_eur: f64,
    /// Only qualifying children attributed to this applicant. Ages stay in the
    /// same benefit band all year; changing bands require a period calculation.
    child_ages: Vec<u8>,
}
fn budget(r: Budget) -> Result<Value, Error> {
    let c = &r.context;
    let assets = cents(r.assets_january1_eur, "assets_january1_eur")?;
    if r.child_ages.is_empty() || r.child_ages.len() > 20 || r.child_ages.iter().any(|a| *a >= 18) {
        return Err(Error::input(
            "child_ages must contain 1 to 20 qualifying children aged 0 through 17",
        ));
    }
    if let Some(v) = review("kindgebonden-budget", c, 2)? {
        return Ok(v);
    }
    let limit = if c.has_partner { 184_633 } else { 146_011 };
    let max = r.child_ages.len() as i64 * 2_580
        + if c.has_partner { 0 } else { 3_416 }
        + r.child_ages
            .iter()
            .map(|age| match age {
                12..=15 => 724,
                16..=17 => 964,
                _ => 0,
            })
            .sum::<i64>();
    let threshold = if c.has_partner { 39_141 } else { 29_736 };
    let reduction = (c.income() - threshold).max(0) * 76_000;
    let reason = (assets > limit * 100).then_some("assets_above_limit");
    let annual = if reason.is_some() {
        0
    } else {
        (max * MICRO - reduction).max(0)
    };
    Ok(envelope(
        "kindgebonden-budget",
        c,
        2,
        Some(annual / 12),
        reason,
        json!({"maximum_annual_eur":max,"income_threshold_eur":threshold,"annual_reduction_eur":display(reduction),"asset_limit_eur":limit}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Huur {
    context: Context,
    basic_rent_monthly_eur: f64,
    household_size: u8,
    eldest_age: u8,
    applicant_partner_assets_january1_eur: f64,
    other_resident_assets_january1_eur: Vec<f64>,
    under21_exception: bool,
}
fn huur(r: Huur) -> Result<Value, Error> {
    let c = &r.context;
    let rent = cents(r.basic_rent_monthly_eur, "basic_rent_monthly_eur")? * 10_000;
    if r.eldest_age > 130 {
        return Err(Error::input("eldest_age must be between 0 and 130"));
    }
    let assets = cents(
        r.applicant_partner_assets_january1_eur,
        "applicant_partner_assets_january1_eur",
    )?;
    let other = r
        .other_resident_assets_january1_eur
        .iter()
        .map(|a| cents(*a, "other_resident_assets_january1_eur"))
        .collect::<Result<Vec<_>, _>>()?;
    let adults = if c.has_partner { 2 } else { 1 };
    if r.household_size < adults
        || r.household_size > 20
        || other.len() != (r.household_size - adults) as usize
    {
        return Err(Error::input("household_size must match applicant, partner and other resident asset entries (maximum 20)"));
    }
    if let Some(v) = review("huurtoeslag", c, 1)? {
        return Ok(v);
    }
    if r.eldest_age < 18 || r.under21_exception {
        return Ok(envelope(
            "huurtoeslag",
            c,
            1,
            None,
            Some("special_young_household_requires_review"),
            json!({}),
        ));
    }
    let single = r.household_size == 1;
    let base = if single { 202_520_000 } else { 200_710_000 };
    let cap = if r.eldest_age < 21 {
        498_200_000
    } else {
        932_930_000
    };
    let top = if r.household_size < 3 {
        713_020_000
    } else {
        764_140_000
    };
    let rent = rent.min(cap);
    let a = (rent.min(498_200_000) - base).max(0);
    let b = (rent.min(top) - 498_200_000).max(0) * 65 / 100;
    let d = (rent - top).max(0) * 40 / 100;
    let threshold = if single { 23_425 } else { 31_500 };
    let correction = (c.income() - threshold).max(0) * if single { 270_000 } else { 220_000 } / 12;
    let asset_limit = if c.has_partner { 76_958 } else { 38_479 };
    let reason = (assets > asset_limit * 100 || other.iter().any(|a| *a > 38_479 * 100))
        .then_some("assets_above_limit");
    let monthly = if reason.is_some() {
        0
    } else {
        (a + b + d - correction).max(0)
    };
    Ok(envelope(
        "huurtoeslag",
        c,
        1,
        Some(monthly),
        reason,
        json!({
        "calculation_rent_eur":display(rent),"base_rent_eur":display(base),"part_a_eur":display(a),
        "part_b_eur":display(b),"part_c_eur":display(d),"income_reduction_eur":display(correction),
        "income_threshold_eur":threshold,"asset_limit_applicant_partner_eur":asset_limit,
        "asset_limit_per_other_resident_eur":38_479}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Childcare {
    context: Context,
    children: Vec<Care>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Care {
    kind: String,
    hours_per_month: f64,
    hourly_rate_eur: f64,
}
fn childcare(r: Childcare) -> Result<Value, Error> {
    let c = &r.context;
    if r.children.is_empty() || r.children.len() > 20 {
        return Err(Error::input(
            "children must contain 1 to 20 children, one childcare type per child",
        ));
    }
    let mut costs = Vec::new();
    for (index, child) in r.children.iter().enumerate() {
        let max_rate = match child.kind.as_str() {
            "daycare" => 1123,
            "after_school" => 998,
            "childminder" => 849,
            _ => {
                return Err(Error::input(
                    "kind must be daycare, after_school or childminder",
                ))
            }
        };
        let rate = cents(child.hourly_rate_eur, "hourly_rate_eur")?.min(max_rate);
        let hours = cents(child.hours_per_month, "hours_per_month")?;
        if hours > 74400 {
            return Err(Error::input("hours_per_month must not exceed 744"));
        }
        let hours = hours.min(23000);
        // Hours and rates have two decimals: product is 1/10000 EUR.
        costs.push((index, hours, rate, hours * rate * 100));
    }
    if let Some(v) = review("kinderopvangtoeslag", c, 3)? {
        return Ok(v);
    }
    let bands: Vec<[i64; 4]> = serde_json::from_str(include_str!("../data/childcare-2026.json"))
        .expect("validated embedded table");
    let band = bands
        .iter()
        .find(|b| c.income() >= b[0] && c.income() <= b[1])
        .expect("bounded income");
    let first = costs
        .iter()
        .max_by_key(|(i, h, _, cost)| (*h, *cost, std::cmp::Reverse(*i)))
        .unwrap()
        .0;
    let mut monthly = 0;
    let children: Vec<Value> = costs.iter().map(|(index,hours,rate,cost)| {
        let percent_bps = band[if *index == first {2} else {3}];
        let amount = *cost * percent_bps / 10_000;
        monthly += amount;
        json!({"child_index":index,"first_child":*index==first,"eligible_hours":*hours as f64/100.0,
            "eligible_hourly_rate_eur":*rate as f64/100.0,"reimbursement_percent":percent_bps as f64/100.0,
            "monthly_before_rounding_eur":display(amount)})
    }).collect();
    Ok(envelope(
        "kinderopvangtoeslag",
        c,
        3,
        Some(monthly),
        None,
        json!({"income_band_from_eur":band[0],"income_band_to_eur":band[1],"children":children}),
    ))
}

pub fn answer(path: &str, body: &[u8]) -> Result<Value, Error> {
    match path {
        "/2026/zorgtoeslag/calculate" => zorg(decode(body)?),
        "/2026/huurtoeslag/calculate" => huur(decode(body)?),
        "/2026/kindgebonden-budget/calculate" => budget(decode(body)?),
        "/2026/kinderopvangtoeslag/calculate" => childcare(decode(body)?),
        _ => Err(Error::input(
            "Unsupported calculation route; use a documented 2026 endpoint",
        )),
    }
}

#[cfg(test)]
mod tests;
