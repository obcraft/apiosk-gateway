//! Brief user-facing refusals. Complete diagnostics remain in task actions.
pub(super) fn coverage(fields: &[String], inputs: bool, dutch: bool) -> String {
    let mut unique = Vec::new();
    for field in fields {
        let field = field.replace('_', " ").split_whitespace().collect::<Vec<_>>().join(" ");
        if !field.is_empty() && !unique.iter().any(|seen: &String| seen.eq_ignore_ascii_case(&field)) {
            unique.push(field);
        }
    }
    // Repeated requirement labels add no information (e.g. Employer profile).
    let prefix = unique.first().and_then(|field| field.split_once(": ")).map(|(prefix, _)| format!("{prefix}: "));
    let prefix = prefix.filter(|prefix| unique.iter().all(|field| field.starts_with(prefix)));
    let labels: Vec<_> = unique.iter().take(3)
        .map(|field| prefix.as_ref().map_or(field.as_str(), |prefix| field.trim_start_matches(prefix))).collect();
    let mut summary = labels.join(", ");
    if unique.len() > 3 {
        summary.push_str(&if dutch { format!(" (+{} meer)", unique.len() - 3) } else { format!(" (+{} more)", unique.len() - 3) });
    }
    if summary.is_empty() || summary.chars().count() > 140 {
        return match (inputs, dutch) {
            (true, true) => "Ik kon nog geen plan maken dat al je filters behoudt. Probeer opnieuw. Er is niets uitgevoerd of afgeschreven.",
            (true, false) => "I couldn’t preserve all your filters in a plan. Try again. Nothing was run or charged.",
            (false, true) => "Ik kon niet bevestigen dat de beschikbare bronnen deze vraag kunnen beantwoorden. Er is niets uitgevoerd of afgeschreven.",
            (false, false) => "I couldn’t confirm that the available sources cover this request. Nothing was run or charged.",
        }.into();
    }
    match (inputs, dutch) {
        (true, true) => format!("Ik kon deze filters niet toepassen: {summary}. Probeer opnieuw. Er is niets uitgevoerd of afgeschreven."),
        (true, false) => format!("I couldn’t apply these filters: {summary}. Try again. Nothing was run or charged."),
        (false, true) => format!("Ik kon geen brondekking bevestigen voor: {summary}. Er is niets uitgevoerd of afgeschreven."),
        (false, false) => format!("I couldn’t confirm source coverage for: {summary}. Nothing was run or charged."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_employer_fields_are_short_and_actionable() {
        let fields: Vec<_> = ["overall_rating", "pay_rating", "work_life_balance_rating", "career_opportunities_rating", "management_rating"]
            .repeat(2).iter().map(|field| format!("Employer profile: {field}")).collect();
        let message = coverage(&fields, false, false);
        assert_eq!(message, "I couldn’t confirm source coverage for: overall rating, pay rating, work life balance rating (+2 more). Nothing was run or charged.");
        assert!(message.chars().count() < 250);
        assert_eq!(fields.len(), 10, "keep full task diagnostics untouched");
    }

    #[test]
    fn input_and_dutch_messages_keep_the_correct_next_step() {
        assert_eq!(coverage(&["Monzo: country".into()], true, false),
            "I couldn’t apply these filters: country. Try again. Nothing was run or charged.");
        let message = coverage(&vec!["Werkgever: salaris".into(); 2], false, true);
        assert_eq!(message.matches("salaris").count(), 1);
        assert!(message.contains("brondekking"));
        assert!(message.contains("niets uitgevoerd of afgeschreven"));
    }

    #[test]
    fn very_long_fields_stay_bounded() {
        assert!(coverage(&["x".repeat(500)], false, false).chars().count() < 200);
    }
}
