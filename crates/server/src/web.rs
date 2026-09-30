//! The web client (design §15), compiled into the binary so one executable serves both
//! the API and the UI. The files live in `web/` at the repository root.

use rouille::{Request, Response};

const INDEX_HTML: &str = include_str!("../../../web/index.html");
const APP_CSS: &str = include_str!("../../../web/app.css");
const APP_JS: &str = include_str!("../../../web/app.js");
const RICH_JS: &str = include_str!("../../../web/rich.js");
const FAVICON_SVG: &str = include_str!("../../../web/favicon.svg");

/// Everything the page uses comes from this server: no CDN, no web fonts, no inline
/// scripts or style attributes. Server data is inserted as text, and this is the backstop.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     connect-src 'self'; img-src 'self' data: blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// Serve a static file of the web client, if the request asks for one.
///
/// # Arguments
///
/// * `request` - The incoming request
///
/// # Returns
///
/// The file, or `None` if the request is not for the web client
#[must_use]
pub fn serve(request: &Request) -> Option<Response> {
    if request.method() != "GET" {
        return None;
    }
    let (content_type, body) = match request.url().as_str() {
        "/" | "/index.html" => ("text/html; charset=utf-8", INDEX_HTML),
        "/assets/app.css" => ("text/css; charset=utf-8", APP_CSS),
        "/assets/app.js" => ("text/javascript; charset=utf-8", APP_JS),
        "/assets/rich.js" => ("text/javascript; charset=utf-8", RICH_JS),
        "/assets/favicon.svg" => ("image/svg+xml", FAVICON_SVG),
        _ => return None,
    };
    Some(
        Response::from_data(content_type, body)
            // Files change only with the binary; revalidate so an upgrade is seen at once.
            .with_additional_header("Cache-Control", "no-cache")
            .with_additional_header("X-Content-Type-Options", "nosniff")
            .with_additional_header("Content-Security-Policy", CONTENT_SECURITY_POLICY)
            .with_additional_header("Referrer-Policy", "no-referrer"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(url: &str) -> Option<Response> {
        serve(&Request::fake_http("GET", url, vec![], Vec::new()))
    }

    #[test]
    fn serves_every_asset_with_its_type() {
        for (url, content_type) in [
            ("/", "text/html; charset=utf-8"),
            ("/assets/app.css", "text/css; charset=utf-8"),
            ("/assets/app.js", "text/javascript; charset=utf-8"),
            ("/assets/rich.js", "text/javascript; charset=utf-8"),
            ("/assets/favicon.svg", "image/svg+xml"),
        ] {
            let response = get(url).unwrap();
            let header = response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("Content-Type"));
            assert_eq!(header.map(|(_, value)| value.as_ref()), Some(content_type), "{url}");
            assert!(response.headers.iter().any(|(name, _)| name == "Content-Security-Policy"));
        }
    }

    #[test]
    fn ignores_other_paths_and_methods() {
        assert!(get("/api/v1/cases").is_none());
        assert!(get("/assets/secret.txt").is_none());
        assert!(serve(&Request::fake_http("POST", "/", vec![], Vec::new())).is_none());
    }
}
