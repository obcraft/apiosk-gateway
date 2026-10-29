use super::*;
fn context(income: i64, partner: bool) -> Value {
    json!({"year":2026,"country":"NL","annual_income_eur":income,"has_partner":partner,
        "eligibility_confirmed":true,"stable_full_year":true})
}
fn run(kind: &str, value: Value) -> Value {
    answer(
        &format!("/2026/{kind}/calculate"),
        value.to_string().as_bytes(),
    )
    .unwrap()
}
fn zorg_input() -> Value {
    json!({"context":context(19000,false),"assets_january1_eur":0,
    "applicant_age":30,"applicant_dutch_health_insurance":true,"partner_dutch_health_insurance":false})
}
fn rent(income: i64, partner: bool, size: u8, age: u8, amount: i64) -> Value {
    json!({"context":context(income,partner),"basic_rent_monthly_eur":amount,
        "household_size":size,"eldest_age":age,"applicant_partner_assets_january1_eur":0,
        "other_resident_assets_january1_eur":vec![0; (size-if partner {2} else {1}) as usize],"under21_exception":false})
}
#[test]
fn official_zorg_examples_and_boundaries() {
    assert_eq!(
        run("zorgtoeslag", zorg_input())["data"]["monthly_amount_eur"],
        129
    );
    let mut v = zorg_input();
    v["context"] = context(33200, true);
    assert_eq!(
        run("zorgtoeslag", v.clone())["data"]["monthly_amount_eur"],
        103
    );
    v["assets_january1_eur"] = json!(184633.01);
    assert_eq!(
        run("zorgtoeslag", v)["data"]["reason"],
        "assets_above_limit"
    );
    let mut v = zorg_input();
    v["assets_january1_eur"] = json!(146011);
    assert_eq!(
        run("zorgtoeslag", v.clone())["data"]["monthly_amount_eur"],
        129
    );
    v["context"]["annual_income_eur"] = json!(40858);
    assert_eq!(run("zorgtoeslag", v)["data"]["monthly_amount_eur"], 0);
}
#[test]
fn official_kindgebonden_examples() {
    for (income, partner, ages, monthly) in [
        (45000, true, vec![4, 8], 392),
        (45000, true, vec![13, 16], 533),
        (30000, false, vec![8], 497),
    ] {
        let v = run(
            "kindgebonden-budget",
            json!({"context":context(income,partner),"assets_january1_eur":0,"child_ages":ages}),
        );
        assert_eq!(v["data"]["monthly_amount_eur"], monthly);
    }
}
#[test]
fn official_rent_examples_and_new_2026_caps() {
    assert_eq!(
        run("huurtoeslag", rent(22000, false, 1, 20, 600))["data"]["monthly_amount_eur"],
        295
    );
    assert_eq!(
        run("huurtoeslag", rent(34000, true, 3, 30, 1200))["data"]["monthly_amount_eur"],
        492
    );
    assert_eq!(
        run("huurtoeslag", rent(29000, false, 1, 69, 710))["data"]["monthly_amount_eur"],
        307
    );
    // High rent is capped for computation, no longer disqualifying in 2026.
    assert_eq!(
        run("huurtoeslag", rent(0, false, 1, 30, 1000))["data"]["monthly_amount_eur"],
        run("huurtoeslag", rent(0, false, 1, 30, 9000))["data"]["monthly_amount_eur"]
    );
    assert_eq!(
        run("huurtoeslag", rent(0, false, 1, 30, 190))["data"]["monthly_amount_eur"],
        0
    );
    assert_eq!(
        run("huurtoeslag", rent(99000, false, 1, 20, 600))["data"]["monthly_amount_eur"],
        0
    );
    let mut v = rent(0, true, 3, 30, 800);
    v["other_resident_assets_january1_eur"] = json!([38479.01]);
    assert_eq!(
        run("huurtoeslag", v)["data"]["reason"],
        "assets_above_limit"
    );
}
#[test]
fn official_childcare_examples() {
    let cases = [
        (
            60000,
            json!([{"kind":"daycare","hours_per_month":122,"hourly_rate_eur":10.50}]),
            1202,
        ),
        (
            120000,
            json!([{"kind":"after_school","hours_per_month":65,"hourly_rate_eur":10.10},{"kind":"daycare","hours_per_month":87,"hourly_rate_eur":12.45}]),
            1162,
        ),
        (
            40000,
            json!([{"kind":"childminder","hours_per_month":240,"hourly_rate_eur":8.25}]),
            1821,
        ),
        (
            50000,
            json!([{"kind":"childminder","hours_per_month":108,"hourly_rate_eur":9.52}]),
            880,
        ),
    ];
    for (income, children, expected) in cases {
        let v = run(
            "kinderopvangtoeslag",
            json!({"context":context(income,true),"children":children}),
        );
        assert_eq!(v["data"]["monthly_amount_eur"], expected);
    }
}
#[test]
fn childcare_ties_are_broken_by_capped_costs_and_table_is_contiguous() {
    let v = run(
        "kinderopvangtoeslag",
        json!({"context":context(120000,true),"children":[
        {"kind":"after_school","hours_per_month":300,"hourly_rate_eur":30},
        {"kind":"daycare","hours_per_month":230,"hourly_rate_eur":12}]}),
    );
    assert_eq!(v["data"]["calculation"]["children"][1]["first_child"], true);
    let bands: Vec<[i64; 4]> =
        serde_json::from_str(include_str!("../../data/childcare-2026.json")).unwrap();
    assert_eq!(bands.len(), 69);
    assert_eq!(bands[0][0], 0);
    assert_eq!(bands.last().unwrap()[1], 99999999);
    for pair in bands.windows(2) {
        assert_eq!(pair[0][1] + 1, pair[1][0]);
        assert!(pair[1][2] <= pair[0][2]);
        assert!(pair[1][3] <= pair[0][3]);
    }
    for (income, percent) in [(56412, 96.0), (56413, 95.5), (99999999, 36.5)] {
        let v = run(
            "kinderopvangtoeslag",
            json!({"context":context(income,false),"children":[{"kind":"daycare","hours_per_month":100,"hourly_rate_eur":10}]}),
        );
        assert_eq!(
            v["data"]["calculation"]["children"][0]["reimbursement_percent"],
            percent
        );
    }
}
#[test]
fn no_amount_for_unsupported_or_unconfirmed_situations() {
    for (key, value) in [
        ("year", json!(2027)),
        ("country", json!("BE")),
        ("eligibility_confirmed", json!(false)),
        ("stable_full_year", json!(false)),
    ] {
        let mut v = zorg_input();
        v["context"][key] = value;
        let result = run("zorgtoeslag", v);
        assert_eq!(result["data"]["status"], "requires_review");
        assert!(result["data"]["monthly_amount_eur"].is_null());
    }
}
#[test]
fn missing_and_unknown_fields_are_rejected_not_guessed() {
    let v = zorg_input();
    for key in v.as_object().unwrap().keys() {
        let mut missing = v.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(answer(
            "/2026/zorgtoeslag/calculate",
            missing.to_string().as_bytes()
        )
        .is_err());
    }
    let mut extra = v.clone();
    extra["bsn"] = json!("123456789");
    assert!(answer("/2026/zorgtoeslag/calculate", extra.to_string().as_bytes()).is_err());
    let mut wrong = v;
    wrong["assets_january1_eur"] = json!(-1);
    assert!(answer("/2026/zorgtoeslag/calculate", wrong.to_string().as_bytes()).is_err());
}
