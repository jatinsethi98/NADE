//! HTML → text, and the `cid:` rewrite. Both on `lol_html`.
//!
//! 47% of the real mail in this account has **no `text/plain` part at all**
//! (`backend/testdata/mime/README.md`), so this is not a fallback: it produces
//! the list snippet, the search index, and the text every agent reads under a
//! 50k-token budget.
//!
//! `html2text` is deliberately not a dependency. Measured over the 28 html-only
//! messages in the live sample it emitted 6.5× the text, 21% of it box-drawing
//! glyphs, because it renders for a terminal and marketing mail *is* tables.
//! `docs/PARSER.md` has the table.
//!
//! **Two passes, because one does not work.** `Element::remove()` drops an
//! element's content from the *output*, but a `text!` handler in the same pass
//! still receives those chunks - `TextChunk::removed()` describes the chunk, not
//! its ancestors. A single-pass version silently pours every `<style>` block
//! into `body_text`.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use lol_html::{doc_text, element, html_content::ContentType, rewrite_str, Settings};

/// Elements whose content is never prose.
const DROPPED: &str = "script, style, head, noscript, template, svg, iframe, object";

/// Elements that end a line of text. `before()` on each of these is what stops
/// `Sign in to Claude.ai` + `Click the button below` fusing into the token
/// `Claude.aiClick`.
const BLOCKS: &str = "p, div, br, tr, li, h1, h2, h3, h4, h5, h6, table, thead, tbody, \
                      blockquote, section, article, header, footer, hr, td, th, ul, ol, pre";

/// Private-use sentinel marking a block boundary inside pass 1's HTML output.
/// Stripped from the input first so a message cannot forge one.
const BLOCK_MARK: char = '\u{0001}';

/// Invisible padding marketing mail uses by the hundred for preview text.
/// Mapped to spaces *before* whitespace collapsing, so they collapse away.
const INVISIBLES: [char; 6] = [
    '\u{00A0}', // no-break space
    '\u{200B}', // zero-width space
    '\u{200C}', // zero-width non-joiner
    '\u{200D}', // zero-width joiner
    '\u{FEFF}', // zero-width no-break space / BOM
    '\u{034F}', // combining grapheme joiner
];

/// Extract readable text from an HTML body.
///
/// Never fails: if `lol_html` bails out on genuinely broken markup we fall back
/// to stripping tags crudely rather than losing the message. Link *targets* are
/// dropped and link *text* kept - URLs are noise for both the search index and
/// the model, and `body_html` still holds the real links for "View original".
#[must_use]
pub fn to_text(html: &str) -> String {
    // EDGE (empty input).
    if html.trim().is_empty() {
        return String::new();
    }

    let sanitised = if html.contains(BLOCK_MARK) {
        html.replace(BLOCK_MARK, " ")
    } else {
        html.to_owned()
    };

    let marked = match pass_one(&sanitised) {
        Ok(out) => out,
        Err(error) => {
            tracing::debug!(%error, "html pass 1 bailed out; falling back to a crude strip");
            return normalise(&decode_entities(&strip_tags(&sanitised)));
        }
    };
    let collected = match pass_two(&marked) {
        Ok(out) => out,
        Err(error) => {
            tracing::debug!(%error, "html pass 2 bailed out; falling back to a crude strip");
            strip_tags(&marked)
        }
    };

    normalise(&decode_entities(&collected))
}

/// Pass 1 - drop everything that is not content, and mark block boundaries in
/// the HTML itself.
fn pass_one(html: &str) -> Result<String, lol_html::errors::RewritingError> {
    rewrite_str(
        html,
        Settings::new()
            .append_element_content_handler(element!(DROPPED, |el| {
                el.remove();
                Ok(())
            }))
            .append_element_content_handler(element!(BLOCKS, |el| {
                el.before(&BLOCK_MARK.to_string(), ContentType::Text);
                Ok(())
            }))
            .append_element_content_handler(element!("img", |el| {
                let alt = el.get_attribute("alt").unwrap_or_default().trim().to_owned();
                if alt.is_empty() {
                    el.remove();
                } else {
                    el.replace(&format!(" {alt} "), ContentType::Text);
                }
                Ok(())
            })),
    )
}

/// Pass 2 - collect text only. Everything `<style>`/`<script>` owned is already
/// gone from the input, so a text handler here cannot see it.
fn pass_two(html: &str) -> Result<String, lol_html::errors::RewritingError> {
    let collected = Rc::new(RefCell::new(String::with_capacity(html.len() / 2)));
    let sink = Rc::clone(&collected);
    rewrite_str(
        html,
        Settings::new().append_document_content_handler(doc_text!(move |chunk| {
            sink.borrow_mut().push_str(chunk.as_str());
            Ok(())
        })),
    )?;
    Ok(Rc::try_unwrap(collected)
        .map(RefCell::into_inner)
        .unwrap_or_else(|shared| shared.borrow().clone()))
}

/// Rewrite `cid:` references to the attachment proxy.
///
/// Done at parse time, not response time (`API.md` §2), so the stored
/// `body_html` is already safe to hand to the locked `WKWebView`. A `cid:` we
/// have no attachment for is left alone: a dead reference is a broken image,
/// while a rewritten-but-wrong one is a 404 round trip on every open.
#[must_use]
pub fn rewrite_cid_urls(html: &str, gmail_id: &str, by_content_id: &HashMap<String, String>) -> String {
    if by_content_id.is_empty() || !html.to_ascii_lowercase().contains("cid:") {
        return html.to_owned();
    }

    let url_for = |value: &str| -> Option<String> {
        let rest = value.trim();
        let rest = rest
            .strip_prefix("cid:")
            .or_else(|| rest.strip_prefix("CID:"))
            .or_else(|| {
                rest.get(..4)
                    .filter(|p| p.eq_ignore_ascii_case("cid:"))
                    .and_then(|_| rest.get(4..))
            })?;
        let key = normalise_content_id(rest);
        by_content_id
            .get(&key)
            .map(|att_id| format!("/v1/messages/{gmail_id}/attachments/{att_id}"))
    };

    let rewritten = rewrite_str(
        html,
        Settings::new().append_element_content_handler(element!("*", |el| {
            for attribute in ["src", "background", "poster", "href"] {
                if let Some(value) = el.get_attribute(attribute) {
                    if let Some(url) = url_for(&value) {
                        let _ = el.set_attribute(attribute, &url);
                    }
                }
            }
            // `style="background-image:url(cid:logo@x)"` is common in mail.
            if let Some(style) = el.get_attribute("style") {
                if style.to_ascii_lowercase().contains("cid:") {
                    let replaced = rewrite_cid_in_css(&style, &url_for);
                    if replaced != style {
                        let _ = el.set_attribute("style", &replaced);
                    }
                }
            }
            Ok(())
        })),
    );

    match rewritten {
        Ok(out) => out,
        Err(error) => {
            tracing::debug!(%error, "cid rewrite bailed out; storing the html unchanged");
            html.to_owned()
        }
    }
}

/// `url(cid:xyz)` inside a `style` attribute.
fn rewrite_cid_in_css(style: &str, url_for: &impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(style.len());
    let mut rest = style;
    while let Some(at) = rest.to_ascii_lowercase().find("cid:") {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find([')', '"', '\'', ' ', ';'])
            .unwrap_or(tail.len());
        let reference = &tail[..end];
        match url_for(reference) {
            Some(url) => out.push_str(&url),
            None => out.push_str(reference),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// A `Content-ID` as it appears in a `cid:` URL: brackets stripped, whitespace
/// gone, percent-escapes undone, lowercased for comparison.
#[must_use]
pub fn normalise_content_id(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('<').trim_end_matches('>');
    percent_decode(trimmed).to_lowercase()
}

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .map_or_else(|_| value.to_owned(), std::borrow::Cow::into_owned)
}

/// Last-resort tag stripper for markup `lol_html` refused. Never panics.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut depth = 0usize;
    for character in html.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(character),
            _ => {}
        }
    }
    out
}

/// Whitespace policy, in the order `docs/PARSER.md` validated.
fn normalise(text: &str) -> String {
    // 1. Invisible padding becomes ordinary space, and the block sentinel a
    //    newline.
    let mut flattened = String::with_capacity(text.len());
    for character in text.chars() {
        if character == BLOCK_MARK {
            flattened.push('\n');
        } else if INVISIBLES.contains(&character) {
            flattened.push(' ');
        } else if character == '\r' {
            // Normalise CRLF to LF; a lone CR is a line break too.
            if !flattened.ends_with('\n') {
                flattened.push('\n');
            }
        } else {
            flattened.push(character);
        }
    }

    // 2 + 3. Collapse whitespace runs inside each line, then blank-line runs.
    let mut lines: Vec<String> = Vec::new();
    for line in flattened.split('\n') {
        let collapsed = collapse_spaces(line);
        if collapsed.is_empty() && lines.last().is_some_and(String::is_empty) {
            continue;
        }
        lines.push(collapsed);
    }

    // 4. Trim.
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn collapse_spaces(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_space = false;
    for character in line.chars() {
        if character.is_whitespace() {
            in_space = true;
        } else {
            if in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = false;
            out.push(character);
        }
    }
    out
}

// ------------------------------------------------------------- entities --

/// The named entities that actually appear in mail, plus every numeric form.
///
/// A complete HTML5 table is 2 231 entries; this is the Latin-1 supplement and
/// the punctuation marketing mail uses, which is what the corpus and the live
/// sample contain. An unknown entity is left verbatim rather than eaten, so a
/// literal `&foo;` in a message still reads as `&foo;`.
const NAMED: &[(&str, char)] = &[
    ("amp", '&'), ("lt", '<'), ("gt", '>'), ("quot", '"'), ("apos", '\''),
    ("nbsp", '\u{00A0}'), ("iexcl", '¡'), ("cent", '¢'), ("pound", '£'),
    ("curren", '¤'), ("yen", '¥'), ("brvbar", '¦'), ("sect", '§'), ("uml", '¨'),
    ("copy", '©'), ("ordf", 'ª'), ("laquo", '«'), ("not", '¬'), ("shy", '\u{00AD}'),
    ("reg", '®'), ("macr", '¯'), ("deg", '°'), ("plusmn", '±'), ("sup2", '²'),
    ("sup3", '³'), ("acute", '´'), ("micro", 'µ'), ("para", '¶'), ("middot", '·'),
    ("cedil", '¸'), ("sup1", '¹'), ("ordm", 'º'), ("raquo", '»'), ("frac14", '¼'),
    ("frac12", '½'), ("frac34", '¾'), ("iquest", '¿'), ("Agrave", 'À'),
    ("Aacute", 'Á'), ("Acirc", 'Â'), ("Atilde", 'Ã'), ("Auml", 'Ä'), ("Aring", 'Å'),
    ("AElig", 'Æ'), ("Ccedil", 'Ç'), ("Egrave", 'È'), ("Eacute", 'É'),
    ("Ecirc", 'Ê'), ("Euml", 'Ë'), ("Igrave", 'Ì'), ("Iacute", 'Í'), ("Icirc", 'Î'),
    ("Iuml", 'Ï'), ("ETH", 'Ð'), ("Ntilde", 'Ñ'), ("Ograve", 'Ò'), ("Oacute", 'Ó'),
    ("Ocirc", 'Ô'), ("Otilde", 'Õ'), ("Ouml", 'Ö'), ("times", '×'), ("Oslash", 'Ø'),
    ("Ugrave", 'Ù'), ("Uacute", 'Ú'), ("Ucirc", 'Û'), ("Uuml", 'Ü'), ("Yacute", 'Ý'),
    ("THORN", 'Þ'), ("szlig", 'ß'), ("agrave", 'à'), ("aacute", 'á'), ("acirc", 'â'),
    ("atilde", 'ã'), ("auml", 'ä'), ("aring", 'å'), ("aelig", 'æ'), ("ccedil", 'ç'),
    ("egrave", 'è'), ("eacute", 'é'), ("ecirc", 'ê'), ("euml", 'ë'), ("igrave", 'ì'),
    ("iacute", 'í'), ("icirc", 'î'), ("iuml", 'ï'), ("eth", 'ð'), ("ntilde", 'ñ'),
    ("ograve", 'ò'), ("oacute", 'ó'), ("ocirc", 'ô'), ("otilde", 'õ'), ("ouml", 'ö'),
    ("divide", '÷'), ("oslash", 'ø'), ("ugrave", 'ù'), ("uacute", 'ú'),
    ("ucirc", 'û'), ("uuml", 'ü'), ("yacute", 'ý'), ("thorn", 'þ'), ("yuml", 'ÿ'),
    ("OElig", 'Œ'), ("oelig", 'œ'), ("Scaron", 'Š'), ("scaron", 'š'), ("Yuml", 'Ÿ'),
    ("fnof", 'ƒ'), ("circ", 'ˆ'), ("tilde", '˜'), ("ensp", '\u{2002}'),
    ("emsp", '\u{2003}'), ("thinsp", '\u{2009}'), ("zwnj", '\u{200C}'),
    ("zwj", '\u{200D}'), ("lrm", '\u{200E}'), ("rlm", '\u{200F}'), ("ndash", '–'),
    ("mdash", '—'), ("lsquo", '‘'), ("rsquo", '’'), ("sbquo", '‚'), ("ldquo", '“'),
    ("rdquo", '”'), ("bdquo", '„'), ("dagger", '†'), ("Dagger", '‡'), ("bull", '•'),
    ("hellip", '…'), ("permil", '‰'), ("prime", '′'), ("Prime", '″'),
    ("lsaquo", '‹'), ("rsaquo", '›'), ("oline", '‾'), ("frasl", '⁄'), ("euro", '€'),
    ("trade", '™'), ("larr", '←'), ("uarr", '↑'), ("rarr", '→'), ("darr", '↓'),
    ("harr", '↔'), ("minus", '−'), ("lowast", '∗'), ("radic", '√'), ("infin", '∞'),
    ("ne", '≠'), ("le", '≤'), ("ge", '≥'), ("loz", '◊'), ("spades", '♠'),
    ("clubs", '♣'), ("hearts", '♥'), ("diams", '♦'), ("alpha", 'α'), ("beta", 'β'),
    ("gamma", 'γ'), ("delta", 'δ'), ("pi", 'π'), ("sigma", 'σ'), ("omega", 'ω'),
];

/// Decode HTML entities. `lol_html` hands text through verbatim - its own docs
/// say a chunk "may contain markup, such as HTML/XML entities" - so this is not
/// optional.
#[must_use]
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let table: HashMap<&str, char> = NAMED.iter().copied().collect();

    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < text.len() {
        if bytes[index] != b'&' {
            // Copy one whole char, not one byte: astral codepoints must survive.
            let character = text[index..].chars().next().unwrap_or('\u{FFFD}');
            out.push(character);
            index += character.len_utf8();
            continue;
        }
        // `&` ... `;` within a sane distance, otherwise it is a literal `&`.
        let limit = (index + 32).min(text.len());
        match text[index..limit].find(';') {
            Some(offset) if offset > 1 => {
                let body = &text[index + 1..index + offset];
                if let Some(character) = decode_one(body, &table) {
                    out.push(character);
                    index += offset + 1;
                    continue;
                }
                out.push('&');
                index += 1;
            }
            _ => {
                out.push('&');
                index += 1;
            }
        }
    }
    out
}

fn decode_one(body: &str, table: &HashMap<&str, char>) -> Option<char> {
    if let Some(digits) = body.strip_prefix('#') {
        let code = if let Some(hex) = digits
            .strip_prefix('x')
            .or_else(|| digits.strip_prefix('X'))
        {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            digits.parse::<u32>().ok()?
        };
        // EDGE (unicode): surrogates and out-of-range values are not chars.
        return char::from_u32(code);
    }
    table.get(body).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Criterion J6 - the trap that started all of this. A single-pass version
    /// pours every `<style>` block into `body_text`.
    #[test]
    fn script_and_style_content_never_leaks() {
        let html = "<html><head><style>.a{color:red}</style></head>\
                    <body><script>var x=1;</script><p>Weekly digest</p>\
                    <noscript>enable js</noscript></body></html>";
        let text = to_text(html);
        assert_eq!(text, "Weekly digest", "{text:?}");
        for forbidden in ["color:red", "var x=1", "enable js"] {
            assert!(!text.contains(forbidden), "{forbidden} leaked: {text:?}");
        }
    }

    /// Criterion J6 - the reason we do not use `mail-parser`'s own extractor:
    /// it produces `Claude.aiClick`.
    #[test]
    fn block_boundaries_do_not_fuse_words() {
        let html = "<div>Sign in to Claude.ai</div><div>Click the button below</div>";
        assert_eq!(to_text(html), "Sign in to Claude.ai\nClick the button below");

        let table = "<table><tr><td>Order</td><td>88-2041</td></tr>\
                     <tr><td>Status</td><td>Shipped</td></tr></table>";
        let text = to_text(table);
        assert!(!text.contains("Order88"), "{text:?}");
        assert!(text.contains("Order"), "{text:?}");
        assert!(text.contains("88-2041"), "{text:?}");
    }

    #[test]
    fn entities_are_decoded_and_alt_text_is_kept() {
        let html = r#"<p>Caf&eacute; &amp; co &mdash; &#8364;41.90 &#x1F680;</p>
                      <img src="x.png" alt="Kettle logo"><img src="spacer.gif" alt="">"#;
        let text = to_text(html);
        assert!(text.contains("Café & co — €41.90 🚀"), "{text:?}");
        assert!(text.contains("Kettle logo"), "{text:?}");
        assert!(!text.contains("spacer"), "{text:?}");
    }

    #[test]
    fn unknown_entities_are_left_alone() {
        assert_eq!(decode_entities("A&foo;B"), "A&foo;B");
        assert_eq!(decode_entities("50% &amp; rising"), "50% & rising");
        assert_eq!(decode_entities("a & b"), "a & b");
        // A surrogate is not a char; leave the source text intact.
        assert_eq!(decode_entities("&#xD800;"), "&#xD800;");
        // Astral codepoints survive the byte walk.
        assert_eq!(decode_entities("🚀&#x1F680;🚀"), "🚀🚀🚀");
    }

    /// Criterion J7.
    #[test]
    fn marketing_preview_padding_is_stripped() {
        let padding: String = std::iter::repeat_n('\u{200B}', 200).collect();
        let html = format!("<div>Real text{padding}\u{00A0}\u{FEFF}\u{034F}here</div>");
        let text = to_text(&html);
        assert_eq!(text, "Real text here", "{text:?}");
        for invisible in INVISIBLES {
            assert!(!text.contains(invisible), "{invisible:?} survived");
        }
    }

    #[test]
    fn link_targets_are_dropped_and_link_text_is_kept() {
        let html = r#"<p>Read the <a href="https://example.com/very/long?x=1">annual report</a> now.</p>"#;
        let text = to_text(html);
        assert_eq!(text, "Read the annual report now.");
        assert!(!text.contains("example.com"), "{text:?}");
    }

    /// Criterion J8.
    #[test]
    fn cid_urls_are_rewritten() {
        let mut map = HashMap::new();
        map.insert("logo@example.com".to_owned(), "AttOne".to_owned());
        let html = r#"<img src="cid:logo@example.com"><img src="cid:missing@example.com">
                      <td background="CID:logo@example.com"></td>
                      <div style="background-image:url(cid:logo@example.com);color:red"></div>"#;
        let out = rewrite_cid_urls(html, "18f2a1b3c4d5e6f7", &map);

        assert!(out.contains("/v1/messages/18f2a1b3c4d5e6f7/attachments/AttOne"));
        assert_eq!(
            out.matches("/v1/messages/18f2a1b3c4d5e6f7/attachments/AttOne")
                .count(),
            3,
            "src, background and the css url should all be rewritten: {out}"
        );
        assert!(
            out.contains("cid:missing@example.com"),
            "an unknown cid is left alone: {out}"
        );
        assert!(out.contains("color:red"), "the rest of the style survives");
    }

    #[test]
    fn content_ids_normalise_the_same_from_header_and_url() {
        assert_eq!(normalise_content_id("<Logo@Example.com>"), "logo@example.com");
        assert_eq!(normalise_content_id(" logo%40example.com "), "logo@example.com");
    }

    /// EDGE (empty input).
    #[test]
    fn empty_and_whitespace_html_is_empty_text() {
        assert_eq!(to_text(""), "");
        assert_eq!(to_text("   \n\t "), "");
        assert_eq!(to_text("<div></div>"), "");
    }

    /// A message cannot forge the block sentinel to inject line breaks.
    #[test]
    fn a_forged_block_sentinel_is_neutralised() {
        let text = to_text("<p>a\u{0001}b</p>");
        assert_eq!(text, "a b");
    }

    #[test]
    fn blank_line_runs_collapse_to_one() {
        // Four empty paragraphs are four block marks; the run of blank lines
        // between "one" and "two" collapses to exactly one, never to zero
        // (which would fuse paragraphs) and never to four.
        let html = "<p>one</p><p></p><p></p><p></p><p>two</p>";
        assert_eq!(to_text(html), "one\n\ntwo");
        assert_eq!(normalise("a\n\n\n\n\nb"), "a\n\nb");
        assert_eq!(normalise("\n\n  a  b  \n\n"), "a b");
    }

    /// Never panics, whatever arrives.
    #[test]
    fn malformed_markup_never_panics() {
        for html in [
            "<p>unclosed",
            "<<<>>>",
            "<div><span></div></span>",
            "<img src=cid: alt=>",
            "&#;&#x;&;&",
            "<p>\u{FFFD}\u{0000}</p>",
        ] {
            let _ = to_text(html);
            let _ = rewrite_cid_urls(html, "g", &HashMap::from([("a".into(), "b".into())]));
        }
    }
}
