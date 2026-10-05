//! Plain HTTP, when npanel serves HTTPS itself: the CA's HTTP-01 challenges
//! are answered, and every name npanel serves is sent to the same path over
//! HTTPS. Nothing else is served in the clear.

use std::sync::Arc;

use axum::body::Body;
use http::{header, HeaderValue, Request, Response, StatusCode};

use super::Shared;

const CHALLENGE_PATH: &str = "/.well-known/acme-challenge/";

pub struct Redirect {
    shared: Arc<Shared>,
    /// Named in the redirect unless it is HTTPS's own.
    https_port: u16,
}

impl Redirect {
    pub fn new(shared: Arc<Shared>, https_port: u16) -> Redirect {
        Redirect { shared, https_port }
    }

    pub fn handle(&self, request: &Request<Body>) -> Response<Body> {
        if let Some(token) = request.uri().path().strip_prefix(CHALLENGE_PATH) {
            let key_authorization = self.shared.http_challenges.lock().get(token).map(|(_, key_authorization)| key_authorization.clone());
            return match key_authorization {
                Some(key_authorization) => Response::builder()
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .body(Body::from(key_authorization))
                    .expect("response"),
                None => not_found(),
            };
        }

        // Only to a name npanel serves, so this can't be made to send a
        // visitor anywhere else: the name is the request's to write.
        let hostname = crate::app::request_host(request);
        if !self.shared.names.load().contains(&hostname) {
            return not_found();
        }
        let path = request.uri().path_and_query().map_or("/", |path| path.as_str());
        let location = match self.https_port {
            443 => format!("https://{hostname}{path}"),
            port => format!("https://{hostname}:{port}{path}"),
        };
        let Ok(location) = HeaderValue::try_from(location) else {
            return not_found();
        };
        // 308 rather than 301, so an analytics event POSTed to the wrong
        // scheme is sent on rather than turned into a GET. Browsers cache a
        // permanent redirect with no freshness of its own until they restart;
        // an hour is enough for the redirect and no longer than switching
        // back to a proxy should take to reach visitors.
        Response::builder()
            .status(StatusCode::PERMANENT_REDIRECT)
            .header(header::LOCATION, location)
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(Body::empty())
            .expect("response")
    }
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("Not found."))
        .expect("response")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(redirect: &Redirect, host: &str, path: &str) -> Response<Body> {
        redirect.handle(&Request::builder().uri(path).header("host", host).body(Body::empty()).unwrap())
    }

    #[test]
    fn answers_challenges_and_redirects_only_served_names() {
        let shared = Arc::new(Shared::default());
        shared.names.store(Arc::new(["example.org".to_string()].into()));
        shared.http_challenges.lock().insert("tok".into(), ("example.org".into(), "tok.thumb".into()));
        let redirect = Redirect::new(shared.clone(), 443);

        assert_eq!(get(&redirect, "anything", "/.well-known/acme-challenge/tok").status(), StatusCode::OK);
        assert_eq!(get(&redirect, "example.org", "/.well-known/acme-challenge/nope").status(), StatusCode::NOT_FOUND);

        let response = get(&redirect, "Example.org:80", "/a/b?c=d");
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(response.headers()[header::LOCATION], "https://example.org/a/b?c=d");
        assert_eq!(get(&redirect, "evil.example", "/").status(), StatusCode::NOT_FOUND);

        let elsewhere = Redirect::new(shared, 8443);
        assert_eq!(get(&elsewhere, "example.org", "/").headers()[header::LOCATION], "https://example.org:8443/");
    }
}
