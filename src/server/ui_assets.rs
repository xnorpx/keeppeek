use super::{UI_ASSETS, service_error};
use include_dir::Dir;
use rouille::{
    Request, Response, ResponseBody,
    content_encoding::{self, Encoding},
};
use sha2::{Digest, Sha256};
use std::{borrow::Cow, collections::HashMap, io::Cursor, sync::OnceLock};

const MIN_COMPRESSIBLE_BYTES: usize = 512;
const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const STATIC_CACHE_CONTROL: &str = "public, max-age=3600";
const HTML_CACHE_CONTROL: &str = "no-cache";

struct CachedUiAsset {
    content_type: &'static str,
    raw: &'static [u8],
    etag_identity: String,
    etag_br: String,
    etag_gzip: String,
    compressible: bool,
    br: OnceLock<Option<Box<[u8]>>>,
    gzip: OnceLock<Option<Box<[u8]>>>,
}

impl CachedUiAsset {
    fn new(path: &str, raw: &'static [u8]) -> Self {
        let content_type = path
            .rsplit_once('.')
            .map_or("application/octet-stream", |(_, ext)| {
                rouille::extension_to_mime(ext)
            });
        let hash = etag_digest(raw);
        Self {
            content_type,
            raw,
            etag_identity: format!("\"{hash}\""),
            etag_br: format!("\"{hash}-br\""),
            etag_gzip: format!("\"{hash}-gzip\""),
            compressible: raw.len() >= MIN_COMPRESSIBLE_BYTES && is_compressible_mime(content_type),
            br: OnceLock::new(),
            gzip: OnceLock::new(),
        }
    }

    fn compressed(&'static self, encoding: Encoding) -> Option<&'static [u8]> {
        let slot = match encoding {
            Encoding::Brotli => &self.br,
            Encoding::Gzip => &self.gzip,
        };
        slot.get_or_init(|| {
            let encoded = content_encoding::compress_bytes(encoding, self.raw);
            (encoded.len() < self.raw.len()).then(|| encoded.into_boxed_slice())
        })
        .as_deref()
    }

    fn select_representation(
        &'static self,
        request: &Request,
    ) -> (Option<&'static str>, &'static [u8], &'static str) {
        if self.compressible
            && let Some(encoding) = content_encoding::preferred_encoding(request)
            && let Some(bytes) = self.compressed(encoding)
        {
            let etag = match encoding {
                Encoding::Brotli => self.etag_br.as_str(),
                Encoding::Gzip => self.etag_gzip.as_str(),
            };
            return (Some(encoding.as_header_value()), bytes, etag);
        }
        (None, self.raw, self.etag_identity.as_str())
    }
}

static UI_ASSET_INDEX: OnceLock<HashMap<String, CachedUiAsset>> = OnceLock::new();

pub(super) fn serve(request: &Request) -> Response {
    if request.method() != "GET" {
        return service_error(404, "not found");
    }

    let path = request.url();
    let key = path.trim_start_matches('/');
    let index = asset_index();
    if let Some(asset) = index.get(key) {
        let cache_control = if key.starts_with("_app/immutable/") {
            IMMUTABLE_CACHE_CONTROL
        } else {
            STATIC_CACHE_CONTROL
        };
        return build_response(request, asset, cache_control, true);
    }

    if key.starts_with("_app/") {
        return Response::empty_404().with_no_cache();
    }

    let document = index
        .get("index.html")
        .expect("the compiled UI must include index.html");
    if path == "/share" {
        return build_response(
            request,
            document,
            "no-cache, no-store, must-revalidate",
            false,
        )
        .with_no_cache();
    }
    build_response(request, document, HTML_CACHE_CONTROL, true)
}

fn build_response(
    request: &Request,
    asset: &'static CachedUiAsset,
    cache_control: &'static str,
    allow_etag: bool,
) -> Response {
    let (encoding, body, etag) = asset.select_representation(request);
    let mut headers: Vec<(Cow<'static, str>, Cow<'static, str>)> = Vec::with_capacity(6);
    headers.push(("Cache-Control".into(), cache_control.into()));
    if asset.compressible {
        headers.push(("Vary".into(), "Accept-Encoding".into()));
    }
    if allow_etag {
        headers.push(("ETag".into(), etag.into()));
        if request
            .header("If-None-Match")
            .is_some_and(|header| if_none_match_matches(header, etag))
        {
            return Response {
                status_code: 304,
                headers,
                data: ResponseBody::empty(),
                upgrade: None,
            };
        }
    }
    headers.push(("Content-Type".into(), asset.content_type.into()));
    if let Some(encoding) = encoding {
        headers.push(("Content-Encoding".into(), encoding.into()));
    }
    Response {
        status_code: 200,
        headers,
        // ponytail: borrow the static asset slice directly instead of cloning per request.
        data: ResponseBody::from_reader_and_size(Cursor::new(body), body.len()),
        upgrade: None,
    }
}

fn asset_index() -> &'static HashMap<String, CachedUiAsset> {
    UI_ASSET_INDEX.get_or_init(|| {
        let mut assets = HashMap::new();
        collect_assets(&UI_ASSETS, &mut assets);
        assets
    })
}

fn collect_assets(dir: &Dir<'static>, assets: &mut HashMap<String, CachedUiAsset>) {
    for file in dir.files() {
        let key = file.path().to_string_lossy().replace('\\', "/");
        let asset = CachedUiAsset::new(&key, file.contents());
        assets.insert(key, asset);
    }
    for subdir in dir.dirs() {
        collect_assets(subdir, assets);
    }
}

fn is_compressible_mime(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || content_type.contains("javascript")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("svg")
}

fn etag_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(16);
    for &byte in &digest[..8] {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn if_none_match_matches(header: &str, etag: &str) -> bool {
    header.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    fn header_value<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_ref())
    }

    fn read_body(response: Response) -> Vec<u8> {
        let (mut reader, _) = response.data.into_reader_and_size();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn immutable_assets_compress_cache_immutably_and_revalidate() {
        let js_key = asset_index()
            .iter()
            .find(|(key, asset)| key.starts_with("_app/immutable/") && asset.compressible)
            .map(|(key, _)| key.clone())
            .expect("built UI must include a compressible immutable asset");
        let url = format!("/{js_key}");

        let identity = serve(&Request::fake_http("GET", &url, Vec::new(), Vec::new()));
        assert_eq!(identity.status_code, 200);
        assert_eq!(header_value(&identity, "Content-Encoding"), None);
        assert_eq!(
            header_value(&identity, "Cache-Control"),
            Some(IMMUTABLE_CACHE_CONTROL)
        );
        assert_eq!(header_value(&identity, "Vary"), Some("Accept-Encoding"));
        let raw_bytes = read_body(identity);

        let gzip = serve(&Request::fake_http(
            "GET",
            &url,
            vec![("Accept-Encoding".into(), "gzip".into())],
            Vec::new(),
        ));
        assert_eq!(gzip.status_code, 200);
        assert_eq!(header_value(&gzip, "Content-Encoding"), Some("gzip"));
        let gzip_etag = header_value(&gzip, "ETag").unwrap().to_owned();
        let gzip_bytes = read_body(gzip);
        assert!(gzip_bytes.len() < raw_bytes.len());
        let mut decoded = Vec::new();
        GzDecoder::new(gzip_bytes.as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, raw_bytes);

        let not_modified = serve(&Request::fake_http(
            "GET",
            &url,
            vec![
                ("Accept-Encoding".into(), "gzip".into()),
                ("If-None-Match".into(), gzip_etag),
            ],
            Vec::new(),
        ));
        assert_eq!(not_modified.status_code, 304);
        assert!(read_body(not_modified).is_empty());

        let brotli = serve(&Request::fake_http(
            "GET",
            &url,
            vec![("Accept-Encoding".into(), "br, gzip".into())],
            Vec::new(),
        ));
        assert_eq!(brotli.status_code, 200);
        assert_eq!(header_value(&brotli, "Content-Encoding"), Some("br"));
        assert!(read_body(brotli).len() < raw_bytes.len());
    }

    #[test]
    fn woff2_fonts_skip_compression_and_missing_app_assets_return_404() {
        if let Some(font_key) = asset_index()
            .keys()
            .find(|key| key.ends_with(".woff2"))
            .cloned()
        {
            let response = serve(&Request::fake_http(
                "GET",
                format!("/{font_key}"),
                vec![("Accept-Encoding".into(), "br, gzip".into())],
                Vec::new(),
            ));
            assert_eq!(response.status_code, 200);
            assert_eq!(header_value(&response, "Content-Encoding"), None);
            assert_eq!(header_value(&response, "Vary"), None);
        }

        let missing = serve(&Request::fake_http(
            "GET",
            "/_app/immutable/chunks/does-not-exist.js",
            Vec::new(),
            Vec::new(),
        ));
        assert_eq!(missing.status_code, 404);
    }

    #[test]
    fn share_route_never_emits_etag_or_304() {
        let first = serve(&Request::fake_http("GET", "/share", Vec::new(), Vec::new()));
        assert_eq!(first.status_code, 200);
        assert_eq!(header_value(&first, "ETag"), None);

        let conditional = serve(&Request::fake_http(
            "GET",
            "/share",
            vec![("If-None-Match".into(), "*".into())],
            Vec::new(),
        ));
        assert_eq!(conditional.status_code, 200);
    }
}
