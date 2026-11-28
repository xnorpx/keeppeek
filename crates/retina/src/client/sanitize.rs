// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fmt;
use url::Url;

/// Formats a parsed URL without userinfo or raw query parameters.
pub(super) struct SafeUrl<'a>(pub(super) &'a Url);

impl fmt::Display for SafeUrl<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{}",
            self.0.scheme(),
            self.0.host_str().unwrap_or("<none>")
        )?;
        if let Some(port) = self.0.port() {
            write!(f, ":{port}")?;
        }
        f.write_str(self.0.path())?;
        if self.0.query().is_some() {
            f.write_str("?<query>")?;
        }
        Ok(())
    }
}

/// Sanitizes an arbitrary URL or relative control reference for logs and errors.
pub(super) fn sanitize_url_str(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed == "*" {
        return "*".to_string();
    }
    if let Ok(url) = Url::parse(trimmed) {
        return SafeUrl(&url).to_string();
    }
    redact_unparsed_url(trimmed)
}

fn redact_unparsed_url(raw: &str) -> String {
    let (base, has_query) = match raw.split_once('?') {
        Some((before, _)) => (before, true),
        None => (raw, false),
    };
    let (base, _) = base.split_once('#').unwrap_or((base, ""));
    let mut out = if let Some((scheme, rest)) = base.split_once("://") {
        let auth_end = rest.find('/').unwrap_or(rest.len());
        let host_part = rest[..auth_end]
            .rsplit_once('@')
            .map_or(&rest[..auth_end], |(_, host)| host);
        format!("{scheme}://{host_part}{}", &rest[auth_end..])
    } else {
        let auth_end = base.find('/').unwrap_or(base.len());
        if let Some((_, host)) = base[..auth_end].rsplit_once('@') {
            format!("{host}{}", &base[auth_end..])
        } else {
            base.to_string()
        }
    };
    if has_query {
        out.push_str("?<query>");
    }
    out
}

/// Sanitizes URL values inside an RTSP `RTP-Info` header.
pub(super) fn sanitize_rtp_info(raw: &str) -> String {
    raw.split(',')
        .map(|entry| {
            entry
                .trim()
                .split(';')
                .map(|part| {
                    let part = part.trim();
                    if let Some((key, val)) = part.split_once('=')
                        && key.trim().eq_ignore_ascii_case("url")
                    {
                        format!("{}={}", key.trim(), sanitize_url_str(val))
                    } else {
                        part.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(";")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Scrubs URL credentials/queries and SDP key material before tracing or errors.
pub(super) fn sanitize_sdp(sdp: &str) -> String {
    let mut out = String::with_capacity(sdp.len());
    for segment in sdp.split_inclusive('\n') {
        let (line, ending) = segment.strip_suffix("\r\n").map_or_else(
            || {
                segment
                    .strip_suffix('\n')
                    .map_or((segment, ""), |stripped| (stripped, "\n"))
            },
            |stripped| (stripped, "\r\n"),
        );
        if let Some(uri) = line.strip_prefix("u=") {
            out.push_str("u=");
            out.push_str(&sanitize_url_str(uri));
        } else if let Some(ctrl) = line.strip_prefix("a=control:") {
            out.push_str("a=control:");
            out.push_str(&sanitize_url_str(ctrl));
        } else if line.starts_with("k=") {
            out.push_str("k=[REDACTED]");
        } else if line.starts_with("a=crypto:") {
            out.push_str("a=crypto:[REDACTED]");
        } else {
            out.push_str(line);
        }
        out.push_str(ending);
    }
    out
}
