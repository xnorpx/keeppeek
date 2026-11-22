use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

pub(super) fn for_document(document: &str) -> Option<Arc<str>> {
    if document.len() > 1024 * 1024 {
        return None;
    }
    let mut remaining = document;
    let mut scripts = String::from("'self'");
    let mut count = 0_u32;
    while let Some(start) = remaining.find("<script") {
        count = count.checked_add(1)?;
        if count > 32 {
            return None;
        }
        let tag = &remaining[start + "<script".len()..];
        let body = &tag[tag.find('>')? + 1..];
        let end = body.find("</script>")?;
        let digest = STANDARD.encode(Sha256::digest(&body.as_bytes()[..end]));
        scripts.push_str(&format!(" 'sha256-{digest}'"));
        remaining = &body[end + "</script>".len()..];
    }
    Some(Arc::from(format!(
        "default-src 'none'; script-src {scripts}; style-src 'self' 'unsafe-inline'; \
         connect-src 'self'; media-src 'self'; img-src 'self' data:; font-src 'self'; \
         frame-ancestors 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; \
         worker-src 'none'"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_only_exact_bundled_inline_scripts_and_same_origin_modules() {
        let body = "window.guest = true;\n";
        let document = format!("<script>{body}</script><script src='/app.js'></script>");
        let csp = for_document(&document).unwrap();
        assert!(csp.contains(&format!(
            "'sha256-{}'",
            STANDARD.encode(Sha256::digest(body))
        )));
        assert!(csp.contains("script-src 'self' 'sha256-"));
        assert!(!csp.contains("script-src 'unsafe-inline'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("worker-src 'none'"));
    }

    #[test]
    fn rejects_oversized_or_incomplete_bundles() {
        assert!(for_document(&" ".repeat(1024 * 1024 + 1)).is_none());
        assert!(for_document("<script>unfinished").is_none());
        assert!(for_document(&"<script></script>".repeat(33)).is_none());
        assert!(for_document(&"<script></script>".repeat(32)).is_some());
    }
}
