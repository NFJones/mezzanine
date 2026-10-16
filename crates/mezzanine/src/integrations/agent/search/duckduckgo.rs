//! Legacy DuckDuckGo HTML decoding only. This deliberately preserves the old
//! scanner's stop-on-incomplete-anchor and permissive nonblank-link behavior.
//! It does not claim strict future URL/date/domain semantics or classify empty
//! challenge pages. No snippets, dates, scores or usage are fabricated.

use mez_agent::search::SearchHit;

/// Returns complete legacy anchors in source order, without new URL policy.
pub(super) fn decode(body: &str, max_results: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut remainder = body;
    while hits.len() < max_results {
        let Some(class_pos) = remainder.find("result__a") else {
            break;
        };
        remainder = &remainder[class_pos..];
        let Some(href_attr_pos) = remainder.find("href=\"") else {
            break;
        };
        let href_start = href_attr_pos + "href=\"".len();
        let Some(href_end) = remainder[href_start..].find('"') else {
            break;
        };
        let href = &remainder[href_start..href_start + href_end];
        let Some(title_start) = remainder[href_start + href_end..].find('>') else {
            break;
        };
        let title_start = href_start + href_end + title_start + 1;
        let Some(title_end) = remainder[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&remainder[title_start..title_start + title_end]);
        let href = clean_duckduckgo_href(href);
        if !title.trim().is_empty() && !href.trim().is_empty() {
            hits.push(SearchHit {
                title,
                url: href,
                excerpt: None,
                source_date: None,
            });
        }
        remainder = &remainder[title_start + title_end..];
    }
    hits
}

/// Removes legacy title tags and collapses decoded whitespace.
fn clean_html_text(value: &str) -> String {
    let mut cleaned = String::new();
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => cleaned.push(ch),
            _ => {}
        }
    }
    decode_html_entities(&cleaned)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Unwraps the existing DDG redirect query parameter without imposing new URL
/// validation. Invalid percent encoding retains the old encoded value.
fn clean_duckduckgo_href(value: &str) -> String {
    let decoded = decode_html_entities(value);
    if let Some(encoded) = query_param_value(&decoded, "uddg") {
        return urlencoding::decode(encoded)
            .map(|value| value.into_owned())
            .unwrap_or_else(|_| encoded.to_string());
    }
    decoded
}

/// Finds the first legacy substring query parameter, preserving old quirks.
fn query_param_value<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=");
    let start = url.find(&marker)? + marker.len();
    let rest = &url[start..];
    let end = rest.find('&').unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Decodes exactly the legacy small HTML entity set.
fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}
