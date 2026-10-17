//! Bounded DuckDuckGo result-shape recognition and complete-entry projection.
//!
//! Only real closed anchor tags with an exact result class and same-tag href
//! supply hits. Comments/raw script/style text cannot manufacture results, and
//! malformed or unfinished anchors cannot borrow another tag's attributes.
//! Final validation scans the whole retained body, even after the output count
//! is satisfied; progress may retain an earlier complete prefix provisionally.
//! This is not a general HTML repair engine or challenge detector. Unknown
//! zero-hit pages fail: no genuine completed-empty format is established here.
//! Legacy nonblank links/title cleanup remain, without strict future URL policy.

use mez_agent::search::SearchHit;

/// Complete hits retained before any malformed/incomplete suffix, for progress
/// only. The caller must still classify final HTTP/body completion separately.
pub(super) fn decode(body: &str, max_results: usize) -> Vec<SearchHit> {
    scan(body, max_results).hits
}

/// Accepts a supported result shape only after inspecting every retained tag.
/// The count cap limits projection, not validation or recognition; zero cannot
/// turn an unrecognized response into evidence of provider zero hits.
pub(super) fn completed_results(body: &str, max_results: usize) -> Option<Vec<SearchHit>> {
    let parsed = scan(body, max_results);
    (parsed.recognized && !parsed.invalid).then_some(parsed.hits)
}

/// Bounded prefix projection plus final-shape evidence, never raw diagnostics.
#[derive(Default)]
struct Scan {
    hits: Vec<SearchHit>,
    recognized: bool,
    invalid: bool,
}

/// A borrowed tag's exact source span and attribute scope.
struct Tag<'a> {
    name: &'a str,
    attributes: &'a str,
    start: usize,
    end: usize,
    closing: bool,
    self_closing: bool,
}

/// Scans without browser-style repair. Nonresult page forms disqualify final
/// success but do not erase already observed provisional complete entries.
fn scan(body: &str, max_results: usize) -> Scan {
    let mut parsed = Scan::default();
    let mut cursor = 0;
    let mut anchor_open = false;
    let mut selected: Option<(usize, String)> = None;
    let mut title_tags: Vec<&str> = Vec::new();
    while let Some(offset) = body[cursor..].find('<') {
        let start = cursor + offset;
        if body[start..].starts_with("<!--") {
            let Some(end) = body[start + 4..].find("-->") else {
                parsed.invalid = true;
                break;
            };
            // Do not treat commented markup inside a selected title as text.
            if selected.is_some() {
                parsed.invalid = true;
                break;
            }
            cursor = start + 4 + end + 3;
            continue;
        }
        let Some(tag) = read_tag(body, start) else {
            parsed.invalid = true;
            break;
        };
        cursor = tag.end;
        if !tag.closing
            && [
                "script", "style", "textarea", "title", "template", "noscript",
            ]
            .iter()
            .any(|name| tag.name.eq_ignore_ascii_case(name))
        {
            if selected.is_some() {
                parsed.invalid = true;
                break;
            }
            let Some(end) = raw_text_end(body, cursor, tag.name) else {
                parsed.invalid = true;
                break;
            };
            cursor = end;
            continue;
        }
        if !tag.closing && tag.name.eq_ignore_ascii_case("form") {
            match nonresult_form(tag.attributes) {
                Ok(true) | Err(()) => parsed.invalid = true,
                Ok(false) => {}
            }
        }
        if !tag.name.eq_ignore_ascii_case("a") {
            if selected.is_some() {
                if tag.closing {
                    if !title_tags
                        .pop()
                        .is_some_and(|name| name.eq_ignore_ascii_case(tag.name))
                    {
                        parsed.invalid = true;
                        break;
                    }
                } else if !tag.self_closing
                    && !["br", "img", "wbr", "hr", "input"]
                        .iter()
                        .any(|name| tag.name.eq_ignore_ascii_case(name))
                {
                    if title_tags.len() >= 64 {
                        parsed.invalid = true;
                        break;
                    }
                    title_tags.push(tag.name);
                }
            }
            continue;
        }
        if tag.closing {
            if !tag.attributes.is_empty() || !title_tags.is_empty() {
                parsed.invalid = true;
                break;
            }
            anchor_open = false;
            if let Some((title_start, href)) = selected.take() {
                let title = clean_html_text(&body[title_start..tag.start]);
                if title.trim().is_empty() || href.trim().is_empty() {
                    parsed.invalid = true;
                    break;
                }
                parsed.recognized = true;
                if parsed.hits.len() < max_results {
                    parsed.hits.push(SearchHit {
                        title,
                        url: href,
                        excerpt: None,
                        source_date: None,
                    });
                }
            }
            continue;
        }
        if anchor_open || tag.self_closing {
            parsed.invalid = true;
            break;
        }
        anchor_open = true;
        let Ok(class) = attribute(tag.attributes, "class") else {
            parsed.invalid = true;
            break;
        };
        if class.is_some_and(|value| {
            value
                .split_ascii_whitespace()
                .any(|token| token == "result__a")
        }) {
            let Ok(Some(href)) = attribute(tag.attributes, "href") else {
                parsed.invalid = true;
                break;
            };
            selected = Some((tag.end, clean_duckduckgo_href(href)));
        }
    }
    parsed.invalid |= anchor_open;
    parsed
}

/// Reads one quote-aware tag, without crossing another opening delimiter.
/// All slicing boundaries are ASCII delimiters and therefore UTF-8 boundaries.
fn read_tag(body: &str, start: usize) -> Option<Tag<'_>> {
    let mut quote = None;
    let mut end = None;
    for (offset, byte) in body.as_bytes()[start + 1..].iter().copied().enumerate() {
        match (quote, byte) {
            (Some(expected), actual) if actual == expected => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'<') => return None,
            (None, b'>') => {
                end = Some(start + 1 + offset);
                break;
            }
            _ => {}
        }
    }
    let end = end?;
    let mut inner = body[start + 1..end].trim();
    let closing = inner.starts_with('/');
    if closing {
        inner = inner[1..].trim_start();
    }
    let self_closing = inner.ends_with('/');
    if self_closing {
        inner = inner[..inner.len() - 1].trim_end();
    }
    let name_end = inner.find(char::is_whitespace).unwrap_or(inner.len());
    let name = &inner[..name_end];
    if name.is_empty() {
        return None;
    }
    Some(Tag {
        name,
        attributes: inner[name_end..].trim(),
        start,
        end: end + 1,
        closing,
        self_closing,
    })
}

/// Finds a real raw-text closing tag rather than parsing embedded fake anchors.
fn raw_text_end(body: &str, mut cursor: usize, name: &str) -> Option<usize> {
    while let Some(offset) = body[cursor..].find("</") {
        cursor += offset;
        if body[cursor + 2..]
            .get(..name.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(name))
        {
            let tag = read_tag(body, cursor)?;
            if tag.closing && tag.name.eq_ignore_ascii_case(name) {
                return Some(tag.end);
            }
        }
        cursor += 2;
    }
    None
}

/// Reads attributes within this tag only. Duplicated selected attributes or
/// incomplete values are unsupported rather than repaired using a later tag.
fn attribute<'a>(mut attributes: &'a str, requested: &str) -> Result<Option<&'a str>, ()> {
    let mut found = None;
    while !attributes.trim_start().is_empty() {
        attributes = attributes.trim_start();
        let end = attributes
            .find(|ch: char| ch.is_whitespace() || ch == '=')
            .unwrap_or(attributes.len());
        if end == 0 {
            return Err(());
        }
        let name = &attributes[..end];
        attributes = attributes[end..].trim_start();
        let value = if let Some(rest) = attributes.strip_prefix('=') {
            attributes = rest.trim_start();
            if let Some(quote) = attributes
                .chars()
                .next()
                .filter(|ch| matches!(ch, '\'' | '"'))
            {
                attributes = &attributes[1..];
                let end = attributes.find(quote).ok_or(())?;
                let value = &attributes[..end];
                attributes = &attributes[end + 1..];
                if attributes
                    .chars()
                    .next()
                    .is_some_and(|ch| !ch.is_whitespace())
                {
                    return Err(());
                }
                value
            } else {
                let end = attributes
                    .find(char::is_whitespace)
                    .unwrap_or(attributes.len());
                if end == 0 {
                    return Err(());
                }
                let value = &attributes[..end];
                attributes = &attributes[end..];
                value
            }
        } else {
            ""
        };
        if name.eq_ignore_ascii_case(requested) {
            if found.is_some() {
                return Err(());
            }
            found = Some(value);
        }
    }
    Ok(found)
}

/// Rejects explicit nonresult-form shapes without interpreting their cause.
/// Ordinary search forms and inert/title text containing these words remain
/// valid. This intentionally does not claim exhaustive challenge detection.
fn nonresult_form(attributes: &str) -> Result<bool, ()> {
    let id = attribute(attributes, "id")?.unwrap_or("");
    let action = attribute(attributes, "action")?.unwrap_or("");
    let action_path = action.split(['?', '#']).next().unwrap_or("");
    Ok(matches!(id, "challenge-form" | "anomaly-form")
        || matches!(
            action_path,
            "/challenge"
                | "/anomaly.js"
                | "//duckduckgo.com/anomaly.js"
                | "https://duckduckgo.com/anomaly.js"
        ))
}

/// Removes legacy title tags and collapses decoded whitespace.
fn clean_html_text(value: &str) -> String {
    let mut cleaned = String::new();
    let mut cursor = 0;
    while let Some(offset) = value[cursor..].find('<') {
        let start = cursor + offset;
        cleaned.push_str(&value[cursor..start]);
        // The containing anchor scan has already validated every title tag.
        let Some(tag) = read_tag(value, start) else {
            return String::new();
        };
        cursor = tag.end;
    }
    cleaned.push_str(&value[cursor..]);
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
