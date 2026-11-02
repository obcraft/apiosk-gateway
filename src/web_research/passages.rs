//! Select bounded, verbatim passages across the whole readable page. The LLM
//! still decides which evidence answers the question; lexical rank is not a
//! factual verification or a semantic relevance guarantee.
use scraper::ElementRef;
use std::collections::{BTreeSet, HashSet};

#[derive(Debug)]
pub(super) struct Passage {
    heading: String,
    section: String,
    text: String,
}

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| {
            w.chars().count() >= 2
                && ![
                    "what",
                    "which",
                    "who",
                    "when",
                    "where",
                    "why",
                    "how",
                    "are",
                    "the",
                    "and",
                    "for",
                    "of",
                    "is",
                    "an",
                    "to",
                    "me",
                    "about",
                    "with",
                    "against",
                    "does",
                    "do",
                    "has",
                    "have",
                    "in",
                    "on",
                    "at",
                    "from",
                    "its",
                    "their",
                    "this",
                    "that",
                    "can",
                    "could",
                    "you",
                    "please",
                    "show",
                    "find",
                    "give",
                    "tell",
                    "get",
                    "check",
                    "information",
                    "info",
                    "latest",
                    "current",
                    "recent",
                    "developments",
                    "wat",
                    "welke",
                    "wie",
                    "wanneer",
                    "waar",
                    "waarom",
                    "hoe",
                    "zijn",
                    "het",
                    "de",
                    "een",
                    "van",
                    "en",
                    "over",
                    "op",
                    "in",
                    "aan",
                    "voor",
                    "met",
                    "bij",
                    "om",
                    "uit",
                    "die",
                    "dat",
                    "dit",
                    "deze",
                    "kan",
                    "kun",
                    "je",
                    "mij",
                    "wil",
                    "graag",
                    "geef",
                    "zoek",
                    "toon",
                    "vertel",
                    "informatie",
                    "laatste",
                    "huidige",
                    "recente",
                    "ontwikkelingen",
                    "bv",
                    "nv",
                    "ltd",
                    "limited",
                    "inc",
                    "llc",
                    "plc",
                ]
                .contains(&w.as_str())
        })
        .collect()
}

fn block(element: ElementRef<'_>) -> bool {
    matches!(
        element.value().name(),
        "p" | "li"
            | "tr"
            | "blockquote"
            | "pre"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "div"
            | "section"
            | "article"
            | "main"
    )
}

/// Keep section and paragraph boundaries instead of joining unrelated stories.
/// The HTTP layer already caps the HTML body; do not cut it at the first 12k
/// characters before looking for the user's answer farther down the document.
pub(super) fn readable(root: ElementRef<'_>) -> Vec<Passage> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut last = None;
    for node in root.descendants() {
        let Some(text) = node.value().as_text() else {
            continue;
        };
        if node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(super::excluded)
        {
            continue;
        }
        let owner = node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|e| {
                block(*e) && !matches!(e.value().name(), "div" | "section" | "article" | "main")
            })
            .or_else(|| {
                node.ancestors()
                    .filter_map(ElementRef::wrap)
                    .find(|e| block(*e))
            })
            .unwrap_or(root);
        if last != Some(owner.id()) {
            blocks.push((owner.value().name().into(), String::new()));
            last = Some(owner.id());
        }
        let value = &mut blocks.last_mut().unwrap().1;
        value.push(' ');
        value.push_str(text);
    }
    let mut headings: Vec<(u8, String)> = Vec::new();
    let mut passages = Vec::new();
    for (tag, text) in blocks {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        if matches!(tag.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            let level = tag.as_bytes()[1];
            headings.retain(|(previous, _)| *previous < level);
            // Profile facts such as a person's job title can be headings with
            // no paragraph before the next heading. They are still evidence.
            if level > b'1' {
                passages.push(Passage {
                    heading: headings
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>()
                        .join(" — "),
                    section: headings
                        .iter()
                        .filter(|(level, _)| *level > b'1')
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>()
                        .join(" — "),
                    text: text.clone(),
                });
            }
            headings.push((level, text.chars().take(160).collect()));
            continue;
        }
        let heading = headings
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" — ");
        // Whole sentences stay together where possible. A long unstructured
        // paragraph is split at whitespace, never in the middle of UTF-8.
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let end = rest
                .char_indices()
                .nth(1800)
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            let split = if end < rest.len() {
                rest[..end]
                    .rmatch_indices(['.', '!', '?', '。'])
                    .find(|(i, _)| *i >= end / 2)
                    .map(|(i, c)| i + c.len())
                    .or_else(|| rest[..end].rfind(char::is_whitespace))
                    .unwrap_or(end)
            } else {
                end
            };
            let part = rest[..split].trim();
            if !part.is_empty() {
                passages.push(Passage {
                    heading: heading.clone(),
                    section: headings
                        .iter()
                        .filter(|(level, _)| *level > b'1')
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>()
                        .join(" — "),
                    text: part.into(),
                });
            }
            rest = rest[split..].trim();
        }
    }
    passages
}

pub(super) fn size(passages: &[Passage]) -> usize {
    passages.iter().map(|p| p.text.chars().count()).sum()
}

/// Rank matching passages, keep adjacent qualifying context, then restore
/// document order. Omitted spans are explicit so separate facts are not joined.
pub(super) fn select(
    passages: &[Passage],
    question: &str,
    title: &str,
    limit: usize,
    subject: Option<&str>,
) -> Option<(String, bool)> {
    let terms = words(question);
    if terms.is_empty() {
        return None;
    }
    let title_terms = words(subject.unwrap_or(title));
    let body_terms: Vec<_> = passages.iter().map(|p| words(&p.text)).collect();
    // A page-wide title is provenance, not evidence that every related story
    // below it concerns that person or company. Rank local sections only.
    let heading_terms: Vec<_> = passages.iter().map(|p| words(&p.section)).collect();
    let mut focus: BTreeSet<_> = terms.difference(&title_terms).cloned().collect();
    if subject.is_some() {
        // Generic field names are not evidence for a more specific question:
        // an account-number help page does not answer a company ownership or
        // sanctions request. Keep them when they are the entire requested focus.
        let specific: BTreeSet<_> = focus
            .iter()
            .filter(|term| {
                ![
                    "company",
                    "companies",
                    "name",
                    "number",
                    "profile",
                    "data",
                    "record",
                    "retrieve",
                    "legal",
                    "address",
                    "list",
                    "lists",
                    "result",
                    "results",
                    "bedrijf",
                    "naam",
                    "nummer",
                    "profiel",
                    "gegevens",
                    "adres",
                ]
                .contains(&term.as_str())
            })
            .cloned()
            .collect();
        if !specific.is_empty() {
            focus = specific;
        }
    }
    let focused = (subject.is_some() && !focus.is_empty())
        || body_terms
            .iter()
            .chain(&heading_terms)
            .any(|w| !w.is_disjoint(&focus));
    let weights: Vec<_> = terms
        .iter()
        .map(|term| {
            let frequency = body_terms.iter().filter(|w| w.contains(term)).count();
            let weight = 1.0 + ((passages.len() + 1) as f64 / (frequency + 1) as f64).ln();
            (term, weight * if focus.contains(term) { 2.0 } else { 1.0 })
        })
        .collect();
    let mut ranked: Vec<_> = passages
        .iter()
        .enumerate()
        .filter_map(|(index, p)| {
            let body = &body_terms[index];
            let heading = &heading_terms[index];
            if (p.text.chars().count() < 30 && p.heading.is_empty())
                || (focused && body.is_disjoint(&focus) && heading.is_disjoint(&focus))
            {
                return None;
            }
            let score: f64 = weights
                .iter()
                .map(|(term, weight)| {
                    let present = if body.contains(*term) {
                        1.0
                    } else if heading.contains(*term) {
                        0.7
                    } else {
                        0.0
                    };
                    present * weight
                })
                .sum();
            (score > 0.0).then_some((index, score))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let strongest = ranked.first()?.1;
    let mut selected = BTreeSet::new();
    let mut seen = HashSet::new();
    let mut used = 0;
    for (index, score) in ranked {
        if selected.contains(&index) || score < strongest * 0.3 || selected.len() >= 8 {
            continue;
        }
        let p = &passages[index];
        let cost = p.text.chars().count() + p.heading.chars().count() + 10;
        if used + cost > limit || !seen.insert(p.text.to_lowercase()) {
            continue;
        }
        selected.insert(index);
        used += cost;
        // A profile may put its role and dates in short headings/fields before
        // the matching biography paragraph. Keep this adjacent factual context
        // without treating the page title as a match for unrelated articles.
        for prior in (0..index).rev().take(7) {
            let field = &passages[prior];
            if field.text.chars().count() > 40
                || field.heading.is_empty()
                || field.heading.split(" — ").next() != p.heading.split(" — ").next()
            {
                break;
            }
            let cost = field.text.chars().count() + field.heading.chars().count() + 10;
            if used + cost > limit || selected.len() >= 8 {
                break;
            }
            if !selected.contains(&prior) && seen.insert(field.text.to_lowercase()) {
                selected.insert(prior);
                used += cost;
            }
        }
        // Keep a following qualification with its claim, not a later section.
        if let Some(next) = passages.get(index + 1) {
            let lower = next.text.to_lowercase();
            let qualifies = [
                "however", "but ", "although", "this ", "these ", "it ", "they ", "as of ",
                "maar ", "echter", "dit ", "deze ", "het ", "zij ", "per ",
            ]
            .iter()
            .any(|prefix| lower.starts_with(prefix));
            let cost = next.text.chars().count() + 10;
            if qualifies
                && next.heading == p.heading
                && used + cost <= limit
                && selected.len() < 8
                && selected.insert(index + 1)
            {
                used += cost;
                seen.insert(next.text.to_lowercase());
            }
        }
    }
    if selected.is_empty() {
        return None;
    }
    let trimmed = selected.len() < passages.len();
    let mut result = String::new();
    let mut previous = None;
    let mut heading = "";
    for index in selected {
        let p = &passages[index];
        if !result.is_empty() {
            result.push_str(if previous == Some(index - 1) {
                "\n\n"
            } else {
                "\n\n[…]\n\n"
            });
        }
        if !p.heading.is_empty() && heading != p.heading {
            result.push_str(&p.heading);
            result.push('\n');
            heading = &p.heading;
        }
        result.push_str(&p.text);
        previous = Some(index);
    }
    Some((result, trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn select_html(html: &str, question: &str, title: &str) -> Option<(String, bool)> {
        select(
            &readable(scraper::Html::parse_document(html).root_element()),
            question,
            title,
            12_000,
            None,
        )
    }
    #[test]
    fn finds_answer_beyond_old_prefix_and_removes_other_topics() {
        let html = format!("<main><h1>Monzo</h1>{}<section><h2>Ownership</h2><p>Monzo's persons with significant control are recorded in its official filings.</p><p>However, this filing is dated 2024 and may not reflect later changes.</p></section><h2>Holiday tips</h2><p>Monzo customers can enjoy a holiday in the sunshine using a payment card.</p></main>", "<p>Monzo offers colourful cards and convenient everyday shopping features.</p>".repeat(240));
        let (text, trimmed) = select_html(
            &html,
            "Show Monzo persons with significant control",
            "Monzo",
        )
        .unwrap();
        assert!(text.contains("official filings") && text.contains("dated 2024"));
        assert!(!text.contains("colourful") && !text.contains("holiday"));
        assert!(trimmed);
    }
    #[test]
    fn question_changes_selection_and_heading_keeps_local_context() {
        let html = "<h1>Acme</h1><h2>Revenue</h2><p>The total was EUR 42 million in 2025, excluding discontinued activities.</p><h2>Owners</h2><p>Alice holds 60 percent of the shares and voting rights.</p>";
        let (revenue, _) = select_html(html, "Acme revenue 2025", "Acme").unwrap();
        let (owners, _) = select_html(html, "Acme owners", "Acme").unwrap();
        assert!(revenue.contains("EUR 42 million") && !revenue.contains("Alice"));
        assert!(owners.contains("Alice") && !owners.contains("42 million"));
    }
    #[test]
    fn title_and_acronym_substrings_are_not_body_evidence() {
        assert!(select_html("<title>AI compliance</title><p>Sailing holidays and the best boats for an ocean adventure.</p>", "AI compliance", "AI compliance").is_none());
    }
    #[test]
    fn archive_title_does_not_make_unrelated_headlines_relevant() {
        let html = "<h1>Ada Example – founder</h1><h2>Stay tuned with daily stories</h2><h3>New payment cards launched across Asia</h3><p>A bank expands its consumer card business in the region.</p><h3>Digital euro service providers</h3><p>The central bank selected providers for another technology project.</p>";
        assert!(select_html(
            html,
            "What is Ada Example's role at Example Ventures?",
            "Ada Example founder archive"
        )
        .is_none());
    }
    #[test]
    fn company_context_requires_the_requested_topic_not_generic_customer_help() {
        let question = "KYB check for MONZO BANK LIMITED, company number 09446231: show its Companies House profile and persons with significant control, and screen the company name against sanctions lists.";
        let choose = |html: &str, title: &str| {
            select(
                &readable(scraper::Html::parse_document(html).root_element()),
                question,
                title,
                12_000,
                Some("MONZO BANK LIMITED"),
            )
        };
        assert!(choose("<h1>Getting a bank statement</h1><p>Why is an old name or address showing on my statement? Your statement shows your name and address as they were on the final day.</p><h2>Related articles</h2><p>Your account number and sort code are available in the Monzo app.</p>", "Getting a bank statement | Monzo").is_none());
        assert!(choose("<h1>Monzo</h1><p>Monzo Bank offers colourful cards and useful tools for everyday shopping and spending.</p>", "Monzo Bank").is_none());
        let (text, _) = choose("<h1>Monzo</h1><p>Monzo's persons with significant control are recorded in the Companies House register.</p><p>However, this filing covers only the position at the filing date.</p><h2>Customer help</h2><p>Your name and address can be changed in the Monzo app.</p>", "Monzo").unwrap();
        assert!(text.contains("significant control") && text.contains("filing date"));
        assert!(!text.contains("changed in the Monzo app"));
    }
    #[test]
    fn short_identity_requests_and_name_only_context_still_work() {
        let passages = readable(scraper::Html::parse_document("<h1>Monzo</h1><p>Monzo Bank Limited is a British bank. Its company number is 09446231.</p>").root_element());
        assert!(select(
            &passages,
            "Monzo company number",
            "Monzo",
            12_000,
            Some("Monzo")
        )
        .is_some());
        assert!(select(&passages, "Monzo", "Monzo", 12_000, Some("Monzo")).is_some());
    }
    #[test]
    fn short_profile_headings_preserve_the_role_before_a_new_section() {
        let (text, _) = select_html(
            "<h1>Ada Example</h1><h2>Executive Advisor</h2><h2>Location</h2><p>London</p><h2>Year joined</h2><p>2021</p><h2>Background</h2><p>Ada Example supports technology companies with product and company strategy.</p>",
            "Wat is de rol van Ada Example bij Example Ventures?", "Ada Example | Example Ventures",
        ).unwrap();
        assert!(text.contains("Executive Advisor"));
        assert!(text.contains("2021"));
    }
    #[test]
    fn unicode_passages_are_bounded_and_unrelated_gaps_are_visible() {
        let html = format!("<p>{}</p><p>Een uitgebreid verhaal over de beste vakanties aan zee.</p><p>Omzet: de cijfers zijn voorlopig en nog niet gecontroleerd.</p>", "De omzet bedraagt € 20 miljoen. ".repeat(600));
        let (text, trimmed) = select_html(&html, "Wat is de omzet?", "Jaarverslag").unwrap();
        assert!(trimmed && text.chars().count() <= 12_000);
        assert!(text.contains("€") && text.contains("[…]"));
        assert!(!text.contains("vakanties"));
    }
}
