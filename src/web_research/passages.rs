//! Select bounded, verbatim passages across the whole readable page. The LLM
//! still decides which evidence answers the question; lexical rank is not a
//! factual verification or a semantic relevance guarantee.
use scraper::ElementRef;
use std::collections::{BTreeSet, HashSet};

#[derive(Debug)]
pub(super) struct Passage {
    heading: String,
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
) -> Option<(String, bool)> {
    let terms = words(question);
    if terms.is_empty() {
        return None;
    }
    let title_terms = words(title);
    let body_terms: Vec<_> = passages.iter().map(|p| words(&p.text)).collect();
    let heading_terms: Vec<_> = passages.iter().map(|p| words(&p.heading)).collect();
    let focus: BTreeSet<_> = terms.difference(&title_terms).cloned().collect();
    let focused = body_terms
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
            if p.text.chars().count() < 30
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
    fn unicode_passages_are_bounded_and_unrelated_gaps_are_visible() {
        let html = format!("<p>{}</p><p>Een uitgebreid verhaal over de beste vakanties aan zee.</p><p>Omzet: de cijfers zijn voorlopig en nog niet gecontroleerd.</p>", "De omzet bedraagt € 20 miljoen. ".repeat(600));
        let (text, trimmed) = select_html(&html, "Wat is de omzet?", "Jaarverslag").unwrap();
        assert!(trimmed && text.chars().count() <= 12_000);
        assert!(text.contains("€") && text.contains("[…]"));
        assert!(!text.contains("vakanties"));
    }
}
