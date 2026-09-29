//! Cross-origin requests (CORS).
//!
//! The web UI served by this server is same-origin and needs none of this. It matters
//! when a browser page on another origin calls the API, e.g. a UI hosted separately or a
//! development server. Only origins listed in the configuration (`public_url` and
//! `allowed_origins`) get CORS headers; the browser blocks every other origin.

use rouille::{Request, Response};

/// Methods and headers the API accepts from other origins.
const ALLOWED_METHODS: &str = "GET, POST, PUT, DELETE, OPTIONS";
const ALLOWED_HEADERS: &str = "Authorization, Content-Type";
/// How long browsers may cache a preflight answer, in seconds.
const PREFLIGHT_MAX_AGE: &str = "600";

/// Reduce a URL to its origin: lowercase `scheme://host[:port]`, without path or slash.
///
/// # Returns
///
/// The origin, or `None` if the URL is not `http://` or `https://` with a host
#[must_use]
pub fn origin_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    if host.is_empty() || host.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// The origins allowed to call the API from a browser.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cors {
    origins: Vec<String>,
}

impl Cors {
    /// Allow these origins (already normalized with [`origin_of`]).
    #[must_use]
    pub fn new(origins: Vec<String>) -> Self {
        Self { origins }
    }

    /// The request's `Origin` header, if it is one of the allowed origins.
    fn allowed_origin(&self, request: &Request) -> Option<String> {
        let origin = origin_of(request.header("Origin")?)?;
        self.origins.contains(&origin).then_some(origin)
    }

    /// Answer a CORS preflight (`OPTIONS`) request.
    ///
    /// # Returns
    ///
    /// `None` if the request is not a preflight; otherwise a 204 that carries CORS headers
    /// only when the origin is allowed
    #[must_use]
    pub fn preflight(&self, request: &Request) -> Option<Response> {
        if request.method() != "OPTIONS" {
            return None;
        }
        let response = Response::text("").with_status_code(204);
        Some(match self.allowed_origin(request) {
            Some(origin) => response
                .with_additional_header("Access-Control-Allow-Origin", origin)
                .with_additional_header("Access-Control-Allow-Methods", ALLOWED_METHODS)
                .with_additional_header("Access-Control-Allow-Headers", ALLOWED_HEADERS)
                .with_additional_header("Access-Control-Max-Age", PREFLIGHT_MAX_AGE)
                .with_additional_header("Vary", "Origin"),
            None => response,
        })
    }

    /// Add CORS headers to a response when the request comes from an allowed origin.
    #[must_use]
    pub fn apply(&self, request: &Request, response: Response) -> Response {
        match self.allowed_origin(request) {
            Some(origin) => response
                .with_additional_header("Access-Control-Allow-Origin", origin)
                .with_additional_header("Vary", "Origin"),
            None => response,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_ref())
    }

    fn request(method: &str, origin: Option<&str>) -> Request {
        let headers = origin
            .map(|origin| vec![("Origin".to_owned(), origin.to_owned())])
            .unwrap_or_default();
        Request::fake_http(method, "/api/v1/cases", headers, Vec::new())
    }

    fn cors() -> Cors {
        Cors::new(vec!["https://clank.acme.com".to_owned()])
    }

    #[test]
    fn origins_are_normalized() {
        assert_eq!(
            origin_of("https://Clank.Acme.com/").as_deref(),
            Some("https://clank.acme.com")
        );
        assert_eq!(
            origin_of("http://localhost:5173/app?x=1").as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(origin_of("clank.acme.com"), None);
        assert_eq!(origin_of("ftp://clank.acme.com"), None);
        assert_eq!(origin_of("https://user@evil.com"), None);
    }

    #[test]
    fn preflight_from_an_allowed_origin_gets_cors_headers() {
        let response = cors().preflight(&request("OPTIONS", Some("https://clank.acme.com"))).unwrap();

        assert_eq!(response.status_code, 204);
        assert_eq!(
            header(&response, "Access-Control-Allow-Origin"),
            Some("https://clank.acme.com")
        );
        assert_eq!(header(&response, "Access-Control-Allow-Headers"), Some(ALLOWED_HEADERS));
    }

    #[test]
    fn other_origins_get_no_cors_headers() {
        let response = cors().preflight(&request("OPTIONS", Some("https://evil.example"))).unwrap();
        let applied = cors().apply(&request("GET", Some("https://evil.example")), Response::text("x"));

        assert_eq!(header(&response, "Access-Control-Allow-Origin"), None);
        assert_eq!(header(&applied, "Access-Control-Allow-Origin"), None);
    }

    #[test]
    fn only_options_is_a_preflight_and_allowed_responses_are_marked() {
        assert!(cors().preflight(&request("GET", Some("https://clank.acme.com"))).is_none());

        let applied = cors().apply(&request("GET", Some("https://clank.acme.com")), Response::text("x"));

        assert_eq!(
            header(&applied, "Access-Control-Allow-Origin"),
            Some("https://clank.acme.com")
        );
        assert_eq!(header(&applied, "Vary"), Some("Origin"));
    }
}
