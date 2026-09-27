/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A markup scanner detailed enough to colour a source listing. It is deliberately
//! not a conforming HTML tokenizer: it never rewrites or drops anything, so that the
//! runs it returns can be reassembled into the original source.

/// The kind of markup a run of source text belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TokenKind {
    Text,
    Entity,
    /// The name and punctuation of a start or end tag, but not its attributes.
    Tag,
    AttributeName,
    AttributeValue,
    Comment,
    Doctype,
    Cdata,
    ProcessingInstruction,
    MarkupDeclaration,
}

impl TokenKind {
    /// The class used by `view-source.css`, or `None` when the run needs no colour.
    pub(super) fn class(self) -> Option<&'static str> {
        match self {
            Self::Text => None,
            Self::Entity => Some("entity"),
            Self::Tag => Some("tag"),
            Self::AttributeName => Some("attribute-name"),
            Self::AttributeValue => Some("attribute-value"),
            Self::Comment => Some("comment"),
            Self::Doctype => Some("doctype"),
            Self::Cdata => Some("cdata"),
            Self::ProcessingInstruction => Some("processing-instruction"),
            Self::MarkupDeclaration => Some("markup-declaration"),
        }
    }
}

pub(super) struct Token<'a> {
    pub(super) kind: TokenKind,
    pub(super) text: &'a str,
}

/// Splits `source` into runs which, concatenated in order, are exactly `source`.
pub(super) fn tokenize(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut text_start = 0;
    let mut index = 0;

    while index < bytes.len() {
        if !matches!(bytes[index], b'<' | b'&') {
            index += 1;
            continue;
        }

        let mut run = Vec::new();
        let scanned = if bytes[index] == b'<' {
            read_markup(source, index, &mut run)
        } else {
            read_entity(source, index, &mut run)
        };
        let Some(end) = scanned else {
            index += 1;
            continue;
        };

        push_text(&mut tokens, &source[text_start..index]);
        tokens.append(&mut run);
        index = end;
        text_start = end;
    }

    push_text(&mut tokens, &source[text_start..]);
    tokens
}

fn push_text<'a>(tokens: &mut Vec<Token<'a>>, text: &'a str) {
    if !text.is_empty() {
        tokens.push(Token {
            kind: TokenKind::Text,
            text,
        });
    }
}

/// A character reference such as `&amp;` or `&#169;`.
fn read_entity<'a>(source: &'a str, start: usize, tokens: &mut Vec<Token<'a>>) -> Option<usize> {
    /// Long enough for every named reference, short enough that a stray `&` in prose
    /// does not scan to the far end of the document.
    const LONGEST_REFERENCE: usize = 32;

    let bytes = source.as_bytes();
    let name_start = if bytes.get(start + 1) == Some(&b'#') {
        start + 2
    } else {
        start + 1
    };

    let mut index = name_start;
    while index < bytes.len() && index - start < LONGEST_REFERENCE {
        match bytes[index] {
            b';' if index > name_start => {
                tokens.push(Token {
                    kind: TokenKind::Entity,
                    text: &source[start..=index],
                });
                return Some(index + 1);
            },
            byte if byte.is_ascii_alphanumeric() => index += 1,
            _ => return None,
        }
    }

    None
}

/// Anything introduced by `<`, or `None` when the `<` is just text.
fn read_markup<'a>(source: &'a str, start: usize, tokens: &mut Vec<Token<'a>>) -> Option<usize> {
    let bytes = source.as_bytes();
    let rest = &source[start..];

    if rest.starts_with("<!--") {
        return Some(push_until(source, start, "-->", TokenKind::Comment, tokens));
    }
    if rest.starts_with("<![CDATA[") {
        return Some(push_until(source, start, "]]>", TokenKind::Cdata, tokens));
    }
    if rest.starts_with("<?") {
        let kind = TokenKind::ProcessingInstruction;
        return Some(push_until(source, start, ">", kind, tokens));
    }
    if bytes[start..].len() >= b"<!doctype".len() &&
        bytes[start..start + b"<!doctype".len()].eq_ignore_ascii_case(b"<!doctype")
    {
        return Some(push_until(source, start, ">", TokenKind::Doctype, tokens));
    }
    if rest.starts_with("<!") {
        let kind = TokenKind::MarkupDeclaration;
        return Some(push_until(source, start, ">", kind, tokens));
    }
    if rest.starts_with("</") {
        return bytes
            .get(start + 2)?
            .is_ascii_alphabetic()
            .then(|| push_until(source, start, ">", TokenKind::Tag, tokens));
    }

    bytes
        .get(start + 1)?
        .is_ascii_alphabetic()
        .then(|| read_start_tag(source, start, tokens))
}

/// Pushes one run reaching just past `terminator`, or to the end of an unterminated source.
fn push_until<'a>(
    source: &'a str,
    start: usize,
    terminator: &str,
    kind: TokenKind,
    tokens: &mut Vec<Token<'a>>,
) -> usize {
    let search_start = start + 1;
    let end = source[search_start..]
        .find(terminator)
        .map_or(source.len(), |offset| {
            search_start + offset + terminator.len()
        });
    tokens.push(Token {
        kind,
        text: &source[start..end],
    });
    end
}

/// `<name attr="value">`, colouring the name, each attribute name and each value apart.
///
/// For `<script>` / `<style>`, the following raw text is kept as [`TokenKind::Text`] until
/// the matching end tag, matching Firefox's source view.
fn read_start_tag<'a>(source: &'a str, start: usize, tokens: &mut Vec<Token<'a>>) -> usize {
    let bytes = source.as_bytes();
    let name_end = run_end(bytes, start + 1, is_tag_name_byte);
    let name = &source[start + 1..name_end];
    let raw_text_end_tag = raw_text_element_end_tag(name);
    tokens.push(Token {
        kind: TokenKind::Tag,
        text: &source[start..name_end],
    });

    let mut index = name_end;
    while index < bytes.len() {
        index = push_run(source, index, is_tag_separator_byte, TokenKind::Tag, tokens);
        let Some(&byte) = bytes.get(index) else { break };
        if byte == b'>' {
            tokens.push(Token {
                kind: TokenKind::Tag,
                text: &source[index..index + 1],
            });
            index += 1;
            break;
        }

        let attr_end = run_end(bytes, index, is_attribute_name_byte);
        if attr_end == index {
            // Only `=` can reach this, everything else being a name byte or handled
            // above, so advancing by one byte stays on a character boundary.
            tokens.push(Token {
                kind: TokenKind::Tag,
                text: &source[index..index + 1],
            });
            index += 1;
            continue;
        }
        tokens.push(Token {
            kind: TokenKind::AttributeName,
            text: &source[index..attr_end],
        });
        index = attr_end;

        let equals = run_end(bytes, index, |byte| byte.is_ascii_whitespace());
        if bytes.get(equals) != Some(&b'=') {
            continue;
        }
        tokens.push(Token {
            kind: TokenKind::Tag,
            text: &source[index..=equals],
        });
        index = push_run(
            source,
            equals + 1,
            |byte| byte.is_ascii_whitespace(),
            TokenKind::Tag,
            tokens,
        );
        index = read_attribute_value(source, index, tokens);
    }

    if let Some(end_tag) = raw_text_end_tag {
        index = push_raw_text(source, index, end_tag, tokens);
    }
    index
}

fn raw_text_element_end_tag(name: &str) -> Option<&'static [u8]> {
    if name.eq_ignore_ascii_case("script") {
        Some(b"</script")
    } else if name.eq_ignore_ascii_case("style") {
        Some(b"</style")
    } else {
        None
    }
}

/// Pushes the body of a `<script>` / `<style>` as text, then the closing tag as markup.
fn push_raw_text<'a>(
    source: &'a str,
    start: usize,
    end_tag_prefix: &[u8],
    tokens: &mut Vec<Token<'a>>,
) -> usize {
    let bytes = source.as_bytes();
    let mut index = start;
    while index < bytes.len() {
        if bytes[index..].len() >= end_tag_prefix.len() &&
            bytes[index..index + end_tag_prefix.len()].eq_ignore_ascii_case(end_tag_prefix)
        {
            let after_name = index + end_tag_prefix.len();
            let tag_end = match bytes[after_name..]
                .iter()
                .position(|&byte| byte == b'>')
            {
                Some(offset) => after_name + offset + 1,
                None => break,
            };
            // Only a real end tag: optional whitespace/slash before `>`.
            if bytes[after_name..tag_end - 1]
                .iter()
                .all(|&byte| byte.is_ascii_whitespace() || byte == b'/')
            {
                push_text(tokens, &source[start..index]);
                tokens.push(Token {
                    kind: TokenKind::Tag,
                    text: &source[index..tag_end],
                });
                return tag_end;
            }
        }
        index += 1;
        while index < bytes.len() && !source.is_char_boundary(index) {
            index += 1;
        }
    }

    push_text(tokens, &source[start..]);
    source.len()
}

/// A quoted or unquoted attribute value, quotes included.
fn read_attribute_value<'a>(
    source: &'a str,
    start: usize,
    tokens: &mut Vec<Token<'a>>,
) -> usize {
    let end = match source.as_bytes().get(start) {
        Some(&quote) if quote == b'"' || quote == b'\'' => source[start + 1..]
            .find(quote as char)
            .map_or(source.len(), |offset| start + offset + 2),
        _ => run_end(source.as_bytes(), start, |byte| {
            !byte.is_ascii_whitespace() && byte != b'>'
        }),
    };

    if end > start {
        tokens.push(Token {
            kind: TokenKind::AttributeValue,
            text: &source[start..end],
        });
    }
    end
}

fn is_tag_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
}

fn is_tag_separator_byte(byte: u8) -> bool {
    byte.is_ascii_whitespace() || byte == b'/'
}

fn is_attribute_name_byte(byte: u8) -> bool {
    !byte.is_ascii_whitespace() && !matches!(byte, b'=' | b'>' | b'/')
}

fn run_end(bytes: &[u8], start: usize, accept: impl Fn(u8) -> bool) -> usize {
    let mut index = start;
    while index < bytes.len() && accept(bytes[index]) {
        index += 1;
    }
    index
}

fn push_run<'a>(
    source: &'a str,
    start: usize,
    accept: impl Fn(u8) -> bool,
    kind: TokenKind,
    tokens: &mut Vec<Token<'a>>,
) -> usize {
    let end = run_end(source.as_bytes(), start, accept);
    if end > start {
        tokens.push(Token {
            kind,
            text: &source[start..end],
        });
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(TokenKind, &str)> {
        tokenize(source)
            .into_iter()
            .map(|token| (token.kind, token.text))
            .collect()
    }

    /// The listing shows the source as it was served, so the runs must lose nothing.
    #[test]
    fn tokens_reassemble_the_source() {
        for source in [
            "",
            "plain text",
            "<!DOCTYPE html>\n<p class=\"a\">Hi &amp; bye</p>\n",
            "<!-- unterminated comment",
            "<p title='a > b' hidden>",
            "a < b && c",
            "<?xml version=\"1.0\"?><![CDATA[raw]]>",
            "<p data-é=\"ü\">é</p>",
            "<script>if (a < b) {}</script>",
        ] {
            let reassembled: String = tokenize(source)
                .into_iter()
                .map(|token| token.text)
                .collect();
            assert_eq!(reassembled, source);
        }
    }

    #[test]
    fn start_tag_splits_name_attributes_and_values() {
        assert_eq!(
            kinds("<a href=\"/x\" hidden>"),
            vec![
                (TokenKind::Tag, "<a"),
                (TokenKind::Tag, " "),
                (TokenKind::AttributeName, "href"),
                (TokenKind::Tag, "="),
                (TokenKind::AttributeValue, "\"/x\""),
                (TokenKind::Tag, " "),
                (TokenKind::AttributeName, "hidden"),
                (TokenKind::Tag, ">"),
            ]
        );
    }

    #[test]
    fn declarations_and_comments_are_single_runs() {
        assert_eq!(
            kinds("<!DOCTYPE html><!-- note --><![CDATA[x]]><?pi?>"),
            vec![
                (TokenKind::Doctype, "<!DOCTYPE html>"),
                (TokenKind::Comment, "<!-- note -->"),
                (TokenKind::Cdata, "<![CDATA[x]]>"),
                (TokenKind::ProcessingInstruction, "<?pi?>"),
            ]
        );
    }

    #[test]
    fn only_complete_character_references_are_entities() {
        assert_eq!(
            kinds("&amp;&#169; a & b"),
            vec![
                (TokenKind::Entity, "&amp;"),
                (TokenKind::Entity, "&#169;"),
                (TokenKind::Text, " a & b"),
            ]
        );
    }

    #[test]
    fn a_bare_less_than_sign_stays_text() {
        assert_eq!(kinds("1 < 2"), vec![(TokenKind::Text, "1 < 2")]);
    }

    #[test]
    fn script_and_style_bodies_are_not_highlighted() {
        assert_eq!(
            kinds("<script>if (a < b) {}</script><style>a > b {}</style>"),
            vec![
                (TokenKind::Tag, "<script"),
                (TokenKind::Tag, ">"),
                (TokenKind::Text, "if (a < b) {}"),
                (TokenKind::Tag, "</script>"),
                (TokenKind::Tag, "<style"),
                (TokenKind::Tag, ">"),
                (TokenKind::Text, "a > b {}"),
                (TokenKind::Tag, "</style>"),
            ]
        );
    }
}
