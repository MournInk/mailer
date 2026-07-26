//! Finding the trackers in a mail.
//!
//! CONTRACT:
//! - [`scan`] reads one HTML body and returns the remote resources in it,
//!   grouped by host, each marked with why it looks like a tracker.
//! - It never fetches anything. Deciding what a URL is for is done by looking at
//!   it, which is the whole point: a request is the thing being avoided.
//!
//! WHY THIS IS IN THE BACKEND
//!
//! The reading pane already refuses to load remote resources, and it could count
//! them as it renders. But then the count would only exist for mail somebody
//! opened, and "how much of this was tracking" is a question about everything
//! that arrived. Scanning at sync time answers it for the whole mailbox, and the
//! reading pane gets to show a list it did not have to derive.
//!
//! WHAT A TRACKER LOOKS LIKE
//!
//! A tracking pixel is an image nobody is meant to see, whose only job is to be
//! requested. The signals, in the order they are trusted:
//!
//! 1. **A known host.** The analytics and ESP endpoints below exist for this.
//! 2. **A 1×1 image**, or one styled down to nothing. Declared in the markup, so
//!    it costs nothing to read.
//! 3. **A path that says so** — `/open`, `/pixel`, `/beacon`, `/track`.
//!
//! Anything remote that matches none of those is reported as a plain remote
//! resource: it is still a request that would tell someone the mail was opened,
//! and the reading pane blocks it either way. Calling it a tracker without
//! evidence would make the label worthless.

use std::collections::BTreeMap;

use crate::types::{TrackerHit, TrackerKind};

/// Hosts whose business is knowing you opened the mail. Matched on the
/// registrable suffix, so `t.sendgrid.net` and `u1234.ct.sendgrid.net` both hit.
const KNOWN_HOSTS: &[&str] = &[
    // ESPs and campaign platforms
    "sendgrid.net",
    "sendgrid.com",
    "mailchimp.com",
    "list-manage.com",
    "mandrillapp.com",
    "sparkpostmail.com",
    "mailgun.org",
    "mailgun.net",
    "postmarkapp.com",
    "customeriomail.com",
    "customer.io",
    "braze.com",
    "sailthru.com",
    "exct.net",
    "exacttarget.com",
    "responsys.net",
    "eloqua.com",
    "marketo.com",
    "mktoresp.com",
    "pardot.com",
    "hubspot.com",
    "hs-sites.com",
    "klaviyomail.com",
    "klaviyo.com",
    "iterable.com",
    "intercom-mail.com",
    "intercomcdn.com",
    "convertkit-mail.com",
    "activehosted.com",
    "aweber.com",
    "getresponse.com",
    "constantcontact.com",
    "rs6.net",
    "cmail19.com",
    "createsend.com",
    "sendinblue.com",
    "brevo.com",
    "substack.com",
    "beehiiv.com",
    "ghost.io",
    // Analytics and open-tracking services
    "google-analytics.com",
    "googletagmanager.com",
    "doubleclick.net",
    "mixpanel.com",
    "segment.com",
    "segment.io",
    "amplitude.com",
    "heapanalytics.com",
    "matomo.cloud",
    "hotjar.com",
    "fullstory.com",
    "branch.io",
    "appsflyer.com",
    "adjust.com",
    "kochava.com",
    "bit.ly",
    "mailtrack.io",
    "mailtracker.io",
    "streak.com",
    "yesware.com",
    "hubapi.com",
    "boomerangapp.com",
    "sidekickopen.com",
    "getnotify.com",
    "spytrack.com",
    "emltrk.com",
    "did-it.com",
    // Chinese platforms with the same role
    "umeng.com",
    "cnzz.com",
    "51.la",
    "growingio.com",
    "sensorsdata.cn",
    "talkingdata.com",
    "mmstat.com",
    "alicdn.com/tps",
];

/// Path fragments that announce what the request is for.
const TELLING_PATHS: &[&str] = &[
    "/open", "open.gif", "open.png", "/pixel", "pixel.gif", "pixel.png", "/beacon", "/track",
    "/trk", "/tracking", "/imp", "/impression", "/wf/open", "/o.gif", "/t.gif", "/1x1",
    "spacer.gif", "clear.gif", "trans.gif", "/collect", "/stat", "/log?", "/mail/open",
];

/// Remote references in one HTML body, grouped by host.
///
/// Ordered by host so a message's report is stable between runs — the reading
/// pane shows this list, and a list that shuffles on every render is noise.
pub fn scan(html: &str) -> Vec<TrackerHit> {
    let mut found: BTreeMap<(String, TrackerKind), u32> = BTreeMap::new();

    for res in remote_refs(html) {
        let Some(host) = host_of(&res.url) else { continue };
        let kind = classify(&host, &res);
        *found.entry((host, kind)).or_insert(0) += 1;
    }

    found
        .into_iter()
        .map(|((host, kind), count)| TrackerHit { host, kind, count })
        .collect()
}

/// One remote reference, with whatever the markup said about its size.
struct Ref {
    url: String,
    /// Present when the tag declared a width and a height, or styled them.
    tiny: bool,
}

fn classify(host: &str, res: &Ref) -> TrackerKind {
    if KNOWN_HOSTS.iter().any(|k| host_matches(host, k)) {
        return TrackerKind::Known;
    }
    if res.tiny {
        return TrackerKind::Pixel;
    }
    let lower = res.url.to_ascii_lowercase();
    if TELLING_PATHS.iter().any(|p| lower.contains(p)) {
        return TrackerKind::Pixel;
    }
    TrackerKind::Remote
}

/// True when `host` is `suffix` or a subdomain of it.
///
/// A suffix with a path in it (`alicdn.com/tps`) only ever matches on the host
/// part, which is what the caller passes — the path half is checked by
/// [`TELLING_PATHS`] instead. Kept in the list because the host alone is not
/// enough to accuse a CDN.
fn host_matches(host: &str, suffix: &str) -> bool {
    let suffix = suffix.split('/').next().unwrap_or(suffix);
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

/// The host of an absolute http(s) URL, lowercased, without credentials or port.
fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .or_else(|| url.strip_prefix("//"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // `user:pass@host:port` — the host is what is left.
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?.trim().trim_end_matches('.');
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Every remote URL in the body, with a `tiny` flag for the ones the markup
/// itself says are invisible.
///
/// A hand-rolled scan rather than a parser: this runs on every message as it
/// arrives, the shapes worth finding are `src=`, `background=` and `url(` inside
/// a style, and a full HTML parse to reach three attributes would be the most
/// expensive thing in the sync path.
fn remote_refs(html: &str) -> Vec<Ref> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut i = 0usize;

    while i < bytes.len() {
        // Only inside a tag: `src=` in prose is not a request.
        let Some(open) = lower[i..].find('<').map(|p| i + p) else { break };
        let close = lower[open..].find('>').map(|p| open + p).unwrap_or(bytes.len());
        let tag = &html[open..close.min(bytes.len())];
        let tag_lower = &lower[open..close.min(bytes.len())];

        for attr in ["src=", "background=", "poster="] {
            if let Some(url) = attr_value(tag, tag_lower, attr) {
                if is_remote(&url) {
                    out.push(Ref { url, tiny: looks_tiny(tag, tag_lower) });
                }
            }
        }
        // `url(...)` in an inline style, which is how a background pixel hides.
        for url in css_urls(tag) {
            if is_remote(&url) {
                out.push(Ref { url, tiny: looks_tiny(tag, tag_lower) });
            }
        }

        i = close + 1;
    }
    out
}

fn is_remote(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://") || u.starts_with("//")
}

/// The value of one attribute in one tag, quoted or bare.
fn attr_value(tag: &str, tag_lower: &str, attr: &str) -> Option<String> {
    let mut from = 0usize;
    loop {
        let at = tag_lower[from..].find(attr)? + from;
        // Must be a whole attribute name: `data-src=` is not `src=`.
        let before = tag_lower[..at].chars().last();
        if matches!(before, Some(c) if c.is_alphanumeric() || c == '-' || c == '_') {
            from = at + attr.len();
            continue;
        }
        let value = tag[at + attr.len()..].trim_start();
        let mut chars = value.chars();
        return match chars.next() {
            Some(q @ ('"' | '\'')) => {
                let rest = &value[q.len_utf8()..];
                rest.find(q).map(|end| rest[..end].trim().to_string())
            }
            Some(_) => Some(
                value
                    .split([' ', '\t', '\n', '\r', '>'])
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            ),
            None => None,
        };
    }
}

/// `url(...)` targets inside a tag's inline style.
fn css_urls(tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = tag.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(at) = lower[from..].find("url(") {
        let start = from + at + 4;
        let Some(end) = tag[start..].find(')') else { break };
        let raw = tag[start..start + end].trim().trim_matches(['"', '\'']).trim();
        if !raw.is_empty() {
            out.push(raw.to_string());
        }
        from = start + end;
    }
    out
}

/// True when the tag says this image is not meant to be seen: 1×1, zero-sized,
/// or hidden outright.
fn looks_tiny(tag: &str, tag_lower: &str) -> bool {
    let dim = |attr: &str| -> Option<u32> {
        attr_value(tag, tag_lower, attr)?
            .trim()
            .trim_end_matches("px")
            .parse::<u32>()
            .ok()
    };
    let small = |v: Option<u32>| matches!(v, Some(n) if n <= 3);

    if small(dim("width=")) && small(dim("height=")) {
        return true;
    }
    if let Some(style) = attr_value(tag, tag_lower, "style=") {
        let s = style.to_ascii_lowercase().replace(' ', "");
        if s.contains("display:none") || s.contains("visibility:hidden") {
            return true;
        }
        // `width:1px;height:1px` — the same pixel, dressed as CSS.
        let tiny_css = |prop: &str| {
            s.split(';')
                .find_map(|d| d.strip_prefix(prop))
                .and_then(|v| v.trim_end_matches("px").parse::<u32>().ok())
                .is_some_and(|n| n <= 3)
        };
        if tiny_css("width:") && tiny_css("height:") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(html: &str) -> Vec<(String, TrackerKind, u32)> {
        scan(html).into_iter().map(|h| (h.host, h.kind, h.count)).collect()
    }

    /// The shape a real open-tracking pixel takes.
    #[test]
    fn a_one_by_one_image_is_a_pixel() {
        let hits = kinds(
            r#"<p>hello</p><img src="https://cdn.example.com/o/abc" width="1" height="1" alt="">"#,
        );
        assert_eq!(hits, [("cdn.example.com".into(), TrackerKind::Pixel, 1)]);
    }

    /// A host whose entire business is this needs no other evidence.
    #[test]
    fn a_known_host_is_named_whatever_it_looks_like() {
        let hits = kinds(
            r#"<img src="https://u123.ct.sendgrid.net/wf/open?upn=xyz" width="600" height="200">"#,
        );
        assert_eq!(hits, [("u123.ct.sendgrid.net".into(), TrackerKind::Known, 1)]);
        // Not a subdomain of it, despite the substring.
        assert_eq!(
            kinds(r#"<img src="https://notsendgrid.net.evil.com/x.gif" width="9" height="9">"#)[0].1,
            TrackerKind::Remote
        );
    }

    #[test]
    fn a_telling_path_counts_even_at_a_plausible_size() {
        for url in [
            "https://mail.shop.com/track/open.gif?id=1",
            "https://a.example.org/beacon?u=2",
            "https://x.example.org/e/pixel.png",
        ] {
            let hits = kinds(&format!(r#"<img src="{url}" width="20" height="20">"#));
            assert_eq!(hits[0].1, TrackerKind::Pixel, "{url}");
        }
    }

    /// An ordinary image in a newsletter is still a request that reports the
    /// open, so it is listed — but calling it a tracker would cheapen the word.
    #[test]
    fn an_ordinary_remote_image_is_reported_as_remote() {
        let hits = kinds(r#"<img src="https://images.example.com/hero.jpg" width="600">"#);
        assert_eq!(hits, [("images.example.com".into(), TrackerKind::Remote, 1)]);
    }

    /// A pixel hidden in CSS, which is what a blocker that only reads `src` misses.
    #[test]
    fn a_background_pixel_in_a_style_is_found() {
        let hits = kinds(
            r#"<div style="width:1px;height:1px;background:url('https://t.example.net/p?x=1')"></div>"#,
        );
        assert_eq!(hits, [("t.example.net".into(), TrackerKind::Pixel, 1)]);

        let hidden = kinds(
            r#"<img src="https://t2.example.net/q" style="display:none" width="600" height="400">"#,
        );
        assert_eq!(hidden[0].1, TrackerKind::Pixel);
    }

    /// Local and inline payloads make no request, so they are not findings.
    #[test]
    fn local_references_are_not_requests() {
        assert!(kinds(r#"<img src="cid:part1@mail">"#).is_empty());
        assert!(kinds(r#"<img src="data:image/gif;base64,R0lGOD">"#).is_empty());
        assert!(kinds(r#"<img src="/relative/logo.png">"#).is_empty());
        assert!(kinds("<p>没有图片，只有文字。src= 这三个字也不算</p>").is_empty());
        assert!(kinds("").is_empty());
    }

    /// The pane rewrites blocked URLs to `data-src`, and a re-scan of its output
    /// must not read those back as live requests.
    #[test]
    fn a_neutralised_attribute_is_not_counted_again() {
        assert!(kinds(r#"<img data-src="https://t.example.net/p">"#).is_empty());
    }

    /// Several requests to one host are one finding with a count: a newsletter
    /// with forty images from its CDN is one relationship, not forty.
    #[test]
    fn requests_are_grouped_by_host() {
        let html = r#"
            <img src="https://cdn.a.com/1.png" width="600">
            <img src="https://cdn.a.com/2.png" width="600">
            <img src="https://t.b.com/o" width="1" height="1">
        "#;
        let hits = kinds(html);
        assert_eq!(
            hits,
            [
                ("cdn.a.com".into(), TrackerKind::Remote, 2),
                ("t.b.com".into(), TrackerKind::Pixel, 1),
            ]
        );
    }

    #[test]
    fn hosts_are_read_out_of_awkward_urls() {
        assert_eq!(host_of("https://user:pw@Track.Example.COM:8443/p?x=1").unwrap(), "track.example.com");
        assert_eq!(host_of("//cdn.example.com/x.png").unwrap(), "cdn.example.com");
        assert!(host_of("https://localhost/x").is_none(), "no dot, no host");
        assert!(host_of("mailto:a@b.com").is_none());
        assert!(host_of("").is_none());
    }

    /// Unquoted and single-quoted attributes are both legal HTML and both appear
    /// in real mail, which is generated by every templating engine there is.
    #[test]
    fn attribute_quoting_does_not_matter() {
        assert_eq!(kinds("<img src=https://a.example.com/x.png width=1 height=1")[0].1, TrackerKind::Pixel);
        assert_eq!(kinds("<img src='https://b.example.com/x.png'>")[0].0, "b.example.com");
    }
}
