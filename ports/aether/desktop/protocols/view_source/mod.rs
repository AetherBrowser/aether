/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Serves `view-source:<url>` by fetching `<url>` and returning its bytes as a
//! numbered, syntax-coloured listing, in the manner of Firefox's source view.

mod document;
mod markup;

use std::future::Future;
use std::pin::Pin;

use encoding_rs::{Encoding, UTF_8};
use headers::{ContentType, HeaderMapExt};
use mime::Mime;
use servo::ServoUrl;
use servo::protocol_handler::{
    DiscardFetch, DoneChannel, FetchContext, HttpStatus, NetworkError, ProtocolHandler, Request,
    ResourceFetchTiming, Response, ResponseBody, fetch,
};

use crate::window::{VIEWABLE_SOURCE_SCHEMES, VIEW_SOURCE_SCHEME};

#[derive(Default)]
pub struct ViewSourceProtocolHandler {}

impl ProtocolHandler for ViewSourceProtocolHandler {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        let url = request.current_url();
        let Some(viewed_url) = viewed_url(&url) else {
            return Box::pin(std::future::ready(Response::network_error(
                NetworkError::ResourceLoadError("Cannot view the source of this URL".to_owned()),
            )));
        };

        let timing_type = request.timing_type();
        let mut viewed_request = request.clone();
        *viewed_request.current_url_mut() = viewed_url.clone();
        let context = context.clone();

        Box::pin(async move {
            let fetched = fetch(viewed_request, &mut DiscardFetch, &context).await;
            if fetched.is_network_error() {
                return Response::network_error(NetworkError::ResourceLoadError(
                    "Could not load the source of this URL".to_owned(),
                ));
            }

            let fetched = fetched.actual_response();
            let ResponseBody::Done(source) = &*fetched.body.lock() else {
                return Response::network_error(NetworkError::ResourceLoadError(
                    "Could not load the source of this URL".to_owned(),
                ));
            };
            let source = source.clone();
            let viewed_type = fetched.headers.typed_get::<ContentType>().map(Mime::from);
            let listing = document::source_document(
                viewed_url.as_str(),
                &decoded(&source, viewed_type.as_ref()),
                // Without a content type, assume the markup that `view-source:` is
                // overwhelmingly used on.
                viewed_type.as_ref().is_none_or(is_markup),
            );

            // The listing keeps the `view-source:` URL, so that the address bar and the
            // document origin do not become those of the page being viewed.
            let mut response = Response::new(url, ResourceFetchTiming::new(timing_type));
            response.headers.typed_insert(ContentType::html());
            *response.body.lock() = ResponseBody::Done(listing.into_bytes());
            response.status = HttpStatus::default();
            response
        })
    }
}

/// The URL being viewed, or `None` when it is absent or of a scheme this handler refuses.
fn viewed_url(url: &ServoUrl) -> Option<ServoUrl> {
    let viewed = url.as_str().strip_prefix(VIEW_SOURCE_SCHEME)?;
    let viewed = ServoUrl::parse(viewed.strip_prefix(':')?).ok()?;
    VIEWABLE_SOURCE_SCHEMES
        .contains(&viewed.scheme())
        .then_some(viewed)
}

/// The listing is served as UTF-8 whatever the source was encoded in. Encoding is
/// chosen like Firefox: BOM, then `Content-Type` charset, then a `<meta charset>` in
/// the first 1024 bytes, then UTF-8.
fn decoded(source: &[u8], viewed_type: Option<&Mime>) -> String {
    encoding_for(source, viewed_type).decode(source).0.into_owned()
}

fn encoding_for(source: &[u8], viewed_type: Option<&Mime>) -> &'static Encoding {
    if let Some((encoding, _)) = Encoding::for_bom(source) {
        return encoding;
    }
    if let Some(encoding) = viewed_type
        .and_then(|mime| mime.get_param(mime::CHARSET))
        .and_then(|charset| Encoding::for_label(charset.as_str().as_bytes()))
    {
        return encoding;
    }
    if viewed_type.is_none_or(is_markup) &&
        let Some(encoding) = meta_charset(source)
    {
        return encoding;
    }
    UTF_8
}

/// HTML5 looks for a charset declaration in the first 1024 bytes of the file.
fn meta_charset(source: &[u8]) -> Option<&'static Encoding> {
    let head = &source[..source.len().min(1024)];
    let lower: Vec<u8> = head.iter().map(u8::to_ascii_lowercase).collect();
    let mut pos = 0;
    while let Some(rel) = find_bytes(&lower[pos..], b"<meta") {
        let start = pos + rel;
        let end = lower[start..]
            .iter()
            .position(|&byte| byte == b'>')
            .map(|offset| start + offset)?;
        if let Some(encoding) = charset_in_meta_tag(&lower[start..=end]) {
            return Some(encoding);
        }
        pos = end + 1;
    }
    None
}

fn charset_in_meta_tag(tag: &[u8]) -> Option<&'static Encoding> {
    // Prefer an explicit `charset=…` attribute (HTML5).
    if let Some(encoding) = label_after(tag, b"charset") {
        return Some(encoding);
    }
    // Fall back to `http-equiv=content-type` with a charset in `content=…`.
    let http_equiv = attribute_value(tag, b"http-equiv")?;
    if http_equiv != b"content-type" {
        return None;
    }
    label_after(attribute_value(tag, b"content")?, b"charset")
}

fn attribute_value<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut pos = 0;
    while let Some(rel) = find_bytes(&tag[pos..], name) {
        let start = pos + rel;
        // Require a non-name character before the attribute, so `xcharset` does not match.
        if start > 0 && is_attr_name_byte(tag[start - 1]) {
            pos = start + name.len();
            continue;
        }
        let after_name = start + name.len();
        let equals = skip_ascii_whitespace(tag, after_name);
        if tag.get(equals) != Some(&b'=') {
            pos = after_name;
            continue;
        }
        return Some(quoted_or_token_value(tag, skip_ascii_whitespace(tag, equals + 1)));
    }
    None
}

fn label_after(bytes: &[u8], key: &[u8]) -> Option<&'static Encoding> {
    let mut pos = 0;
    while let Some(rel) = find_bytes(&bytes[pos..], key) {
        let start = pos + rel;
        if start > 0 && is_attr_name_byte(bytes[start - 1]) {
            pos = start + key.len();
            continue;
        }
        let after_key = start + key.len();
        let equals = skip_ascii_whitespace(bytes, after_key);
        if bytes.get(equals) != Some(&b'=') {
            pos = after_key;
            continue;
        }
        let label = quoted_or_token_value(bytes, skip_ascii_whitespace(bytes, equals + 1));
        if let Some(encoding) = Encoding::for_label(label) {
            return Some(encoding);
        }
        pos = after_key;
    }
    None
}

fn quoted_or_token_value(bytes: &[u8], start: usize) -> &[u8] {
    match bytes.get(start) {
        Some(&quote) if quote == b'"' || quote == b'\'' => {
            let end = bytes[start + 1..]
                .iter()
                .position(|&byte| byte == quote)
                .map_or(bytes.len(), |offset| start + 1 + offset);
            &bytes[start + 1..end]
        },
        _ => {
            let end = bytes[start..]
                .iter()
                .position(|&byte| {
                    byte.is_ascii_whitespace() || matches!(byte, b';' | b'>' | b'/')
                })
                .map_or(bytes.len(), |offset| start + offset);
            &bytes[start..end]
        },
    }
}

fn skip_ascii_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

fn is_attr_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Whether colouring the source as markup makes sense for this content type.
fn is_markup(viewed_type: &Mime) -> bool {
    viewed_type.subtype() == mime::HTML ||
        viewed_type.subtype() == mime::XML ||
        viewed_type.suffix() == Some(mime::XML)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewed_url_unwraps_the_page_url() {
        let url = ServoUrl::parse("view-source:https://example.com/a?b=c").expect("should parse");
        assert_eq!(
            viewed_url(&url).map(|url| url.to_string()),
            Some("https://example.com/a?b=c".to_owned())
        );
    }

    #[test]
    fn viewed_url_refuses_unviewable_schemes() {
        for url in [
            "view-source:view-source:https://example.com/",
            "view-source:servo:newtab",
            "view-source:",
        ] {
            let parsed = ServoUrl::parse(url).expect("should parse");
            assert!(viewed_url(&parsed).is_none(), "{url} should not be viewable");
        }
    }

    #[test]
    fn source_is_decoded_with_the_declared_charset() {
        let latin1 = [b'c', b'a', b'f', 0xe9];
        let content_type = "text/html; charset=iso-8859-1"
            .parse::<Mime>()
            .expect("should parse");
        assert_eq!(decoded(&latin1, Some(&content_type)), "café");
        assert_eq!(decoded("café".as_bytes(), None), "café");
    }

    #[test]
    fn source_is_decoded_with_a_meta_charset() {
        let mut bytes = b"<!DOCTYPE html><meta charset=iso-8859-1><title>".to_vec();
        bytes.extend_from_slice(&[b'c', b'a', b'f', 0xe9]);
        assert_eq!(decoded(&bytes, None), "<!DOCTYPE html><meta charset=iso-8859-1><title>café");
    }

    #[test]
    fn source_is_decoded_with_a_content_type_meta() {
        let mut bytes =
            b"<meta http-equiv=content-type content=\"text/html; charset=iso-8859-1\">".to_vec();
        bytes.extend_from_slice(&[0xe9]);
        assert_eq!(
            decoded(&bytes, Some(&"text/html".parse().expect("should parse"))),
            "<meta http-equiv=content-type content=\"text/html; charset=iso-8859-1\">é"
        );
    }

    #[test]
    fn bom_overrides_other_charset_hints() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("café".as_bytes());
        let content_type = "text/html; charset=iso-8859-1"
            .parse::<Mime>()
            .expect("should parse");
        assert_eq!(decoded(&bytes, Some(&content_type)), "café");
    }

    #[test]
    fn markup_types_are_highlighted_and_others_are_not() {
        for content_type in ["text/html", "application/xhtml+xml", "image/svg+xml"] {
            let mime = content_type.parse::<Mime>().expect("should parse");
            assert!(is_markup(&mime), "{content_type} should be highlighted");
        }
        for content_type in ["text/css", "application/json", "text/plain"] {
            let mime = content_type.parse::<Mime>().expect("should parse");
            assert!(!is_markup(&mime), "{content_type} should not be highlighted");
        }
    }
}
