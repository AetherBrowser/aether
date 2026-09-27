/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Builds the numbered listing shown for a `view-source:` URL.

use super::markup::{self, Token, TokenKind};

/// Inlined rather than linked, so the listing pulls in no subresources.
const STYLE: &str = include_str!("../../../../../resources/view-source.css");

/// Servo has no CSS counters, so line numbers are written into the markup. They live
/// in their own element so that the stylesheet can set them apart from the source.
pub(super) fn source_document(viewed_url: &str, source: &str, highlight: bool) -> String {
    let title = escaped(viewed_url);
    let line_count = source.split('\n').count().max(1);
    let number_width = line_count.to_string().len().max(2);
    let lines = lines(source, highlight);
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n\
         <title>Source of {title}</title>\n\
         <style>\n{STYLE}</style>\n</head>\n\
         <body><pre id=\"source\" style=\"--line-number-width: {number_width}ch\">\
         {lines}</pre></body>\n</html>\n"
    )
}

/// One block per source line. Nothing separates the blocks, because a newline inside
/// the preformatted listing would show up as a blank line of its own.
fn lines(source: &str, highlight: bool) -> String {
    let tokens = if highlight {
        markup::tokenize(source)
    } else {
        vec![Token {
            kind: TokenKind::Text,
            text: source,
        }]
    };

    let mut lines = String::new();
    let mut number = 1;
    open_line(&mut lines, number);

    for token in tokens {
        for (index, part) in token.text.split('\n').enumerate() {
            if index > 0 {
                lines.push_str("</span>");
                number += 1;
                open_line(&mut lines, number);
            }
            // The parser turns a carriage return back into a line break, which would
            // split CRLF source across two lines.
            push_run(
                &mut lines,
                part.strip_suffix('\r').unwrap_or(part),
                token.kind,
            );
        }
    }

    lines.push_str("</span>");
    lines
}

fn open_line(out: &mut String, number: usize) {
    out.push_str("<span class=\"line\"><span class=\"line-number\">");
    out.push_str(&number.to_string());
    out.push_str("</span>");
}

fn push_run(out: &mut String, text: &str, kind: TokenKind) {
    if text.is_empty() {
        return;
    }
    match kind.class() {
        Some(class) => {
            out.push_str("<span class=\"");
            out.push_str(class);
            out.push_str("\">");
            push_escaped(out, text);
            out.push_str("</span>");
        },
        None => push_escaped(out, text),
    }
}

fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    push_escaped(&mut escaped, text);
    escaped
}

fn push_escaped(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_line_is_numbered() {
        let lines = lines("one\ntwo\n\nfour", false);
        for number in 1..=4 {
            assert!(
                lines.contains(&format!("<span class=\"line-number\">{number}</span>")),
                "line {number} should be numbered in {lines}"
            );
        }
        assert_eq!(lines.matches("class=\"line\"").count(), 4);
    }

    /// A trailing newline ends a line, so `"one\n"` is two line blocks (the second empty).
    #[test]
    fn a_trailing_newline_adds_an_empty_line() {
        assert_eq!(lines("one\n", false).matches("class=\"line\"").count(), 2);
        assert_eq!(lines("one", false).matches("class=\"line\"").count(), 1);
    }

    #[test]
    fn crlf_source_does_not_double_space() {
        let lines = lines("one\r\ntwo", false);
        assert!(!lines.contains('\r'), "{lines}");
        assert_eq!(lines.matches("class=\"line\"").count(), 2);
    }

    #[test]
    fn markup_is_escaped_and_coloured() {
        let lines = lines("<p class=\"a\">x &amp; y</p>", true);
        assert!(lines.contains("<span class=\"tag\">&lt;p</span>"), "{lines}");
        assert!(
            lines.contains("<span class=\"attribute-name\">class</span>"),
            "{lines}"
        );
        assert!(
            lines.contains("<span class=\"entity\">&amp;amp;</span>"),
            "{lines}"
        );
        assert!(
            lines.contains("<span class=\"attribute-value\">&quot;a&quot;</span>"),
            "{lines}"
        );
    }

    #[test]
    fn unhighlighted_source_is_still_escaped() {
        let lines = lines("<p>", false);
        assert!(lines.contains("&lt;p&gt;"), "{lines}");
        assert!(!lines.contains("class=\"tag\""), "{lines}");
    }

    #[test]
    fn a_run_spanning_lines_is_numbered_on_each_line() {
        let lines = lines("<!-- a\nb -->", true);
        assert_eq!(lines.matches("class=\"line\"").count(), 2);
        assert_eq!(lines.matches("class=\"comment\"").count(), 2);
    }

    #[test]
    fn the_title_names_the_viewed_url() {
        let document = source_document("https://example.com/?a=1&b=2", "x", false);
        assert!(
            document.contains("<title>Source of https://example.com/?a=1&amp;b=2</title>"),
            "{document}"
        );
    }

    #[test]
    fn the_gutter_grows_with_the_line_count() {
        let ten = source_document("u", &"x\n".repeat(9), false);
        assert!(
            ten.contains("--line-number-width: 2ch"),
            "ten lines should keep the minimum two-digit gutter: {ten}"
        );

        let thousand = source_document("u", &"x\n".repeat(999), false);
        assert!(
            thousand.contains("--line-number-width: 4ch"),
            "1000 lines need a four-digit gutter: {thousand}"
        );
    }
}
