//! Shared transport for stream provider modules (03 §8.4): one fetch path,
//! one status mapping (ROD-173 taxonomy via `ProviderError::from_status`).
//! Redirects are refused on every request: provider-supplied URLs must stay
//! behind the fetchguard (03 §6.7, a followed 3xx would bypass it) and fixed
//! API endpoints have no business redirecting (ROD-435 rationale).
//! ROD-300 always-on failure diagnostics are not wired: no log sink exists
//! until the TUI shell ticket.

use std::io::Read;
use std::time::Duration;

use super::ProviderError;

/// Response body ceiling (freeze ROD-341): oversize fails the fetch instead
/// of growing unbounded.
pub const MAX_RESP_BYTES: u64 = 4 * 1024 * 1024;
/// Freeze had no wall-clock rail here; adopted from the AniList ROD-262
/// rationale (a silent host must not hang a detached worker).
const DEADLINE: Duration = Duration::from_secs(10);

/// Success policy: allanime wants 200 only; REST providers accept any 2xx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accept {
    OkOnly,
    Any2xx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

pub struct Request<'a> {
    pub method: Method,
    pub url: &'a str,
    /// `(content_type, body)`.
    pub payload: Option<(&'a str, &'a [u8])>,
    pub user_agent: &'a str,
    pub extra_headers: &'a [(&'a str, &'a str)],
    pub accept: Accept,
}

pub struct HttpClient {
    http: reqwest::blocking::Client,
}

impl HttpClient {
    pub fn new() -> Result<HttpClient, ProviderError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(DEADLINE)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ProviderError::Network)?;
        Ok(HttpClient { http })
    }

    pub fn fetch(&self, req: &Request) -> Result<Vec<u8>, ProviderError> {
        let mut builder = match req.method {
            Method::Get => self.http.get(req.url),
            Method::Post => self.http.post(req.url),
        };
        builder = builder.header("User-Agent", req.user_agent);
        for (name, value) in req.extra_headers {
            builder = builder.header(*name, *value);
        }
        if let Some((content_type, body)) = req.payload {
            builder = builder
                .header("Content-Type", content_type)
                .body(body.to_vec());
        }
        let resp = builder.send().map_err(|_| ProviderError::Network)?;
        let status = resp.status().as_u16();
        let ok = match req.accept {
            Accept::OkOnly => status == 200,
            Accept::Any2xx => resp.status().is_success(),
        };
        if !ok {
            return Err(ProviderError::from_status(status));
        }
        let mut buf = Vec::new();
        resp.take(MAX_RESP_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|_| ProviderError::Network)?;
        if buf.len() as u64 > MAX_RESP_BYTES {
            return Err(ProviderError::Decode(
                "response exceeds the 4 MiB cap".into(),
            ));
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{response_with_body, serve_once};

    fn get_against(response: Vec<u8>, accept: Accept) -> Result<Vec<u8>, ProviderError> {
        let url = serve_once(response);
        HttpClient::new().unwrap().fetch(&Request {
            method: Method::Get,
            url: &url,
            payload: None,
            user_agent: "sabigoku-test",
            extra_headers: &[],
            accept,
        })
    }

    #[test]
    fn ok_body_returned() {
        let got = get_against(response_with_body("200 OK", b"hello"), Accept::Any2xx).unwrap();
        assert_eq!(got, b"hello");
    }

    #[test]
    fn ok_only_rejects_other_2xx() {
        let got = get_against(response_with_body("204 No Content", b""), Accept::OkOnly);
        assert!(matches!(got, Err(ProviderError::Http { status: 204 })));

        let got = get_against(response_with_body("204 No Content", b""), Accept::Any2xx);
        assert!(got.is_ok());
    }

    #[test]
    fn status_classes_map_to_taxonomy() {
        let got = get_against(response_with_body("403 Forbidden", b""), Accept::Any2xx);
        assert!(matches!(got, Err(ProviderError::Forbidden { status: 403 })));

        let got = get_against(
            response_with_body("503 Service Unavailable", b""),
            Accept::Any2xx,
        );
        assert!(matches!(got, Err(ProviderError::Server { status: 503 })));

        let got = get_against(response_with_body("404 Not Found", b""), Accept::Any2xx);
        assert!(matches!(got, Err(ProviderError::Http { status: 404 })));
    }

    #[test]
    fn redirect_is_refused_not_followed() {
        let redirect = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let got = get_against(redirect, Accept::Any2xx);
        assert!(matches!(got, Err(ProviderError::Http { status: 302 })));
    }

    #[test]
    fn connection_refused_is_network() {
        let got = HttpClient::new().unwrap().fetch(&Request {
            method: Method::Get,
            url: "http://127.0.0.1:1/x",
            payload: None,
            user_agent: "sabigoku-test",
            extra_headers: &[],
            accept: Accept::Any2xx,
        });
        assert!(matches!(got, Err(ProviderError::Network)));
    }

    #[test]
    fn body_at_cap_accepted_one_over_refused() {
        let at_cap = vec![b'x'; MAX_RESP_BYTES as usize];
        let got = get_against(response_with_body("200 OK", &at_cap), Accept::Any2xx).unwrap();
        assert_eq!(got.len() as u64, MAX_RESP_BYTES);

        let over = vec![b'x'; MAX_RESP_BYTES as usize + 1];
        let got = get_against(response_with_body("200 OK", &over), Accept::Any2xx);
        assert!(matches!(got, Err(ProviderError::Decode(_))));
    }

    #[test]
    fn post_carries_payload_and_headers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 16384];
            let n = std::io::Read::read(&mut sock, &mut buf).unwrap();
            let _ = std::io::Write::write_all(&mut sock, &response_with_body("200 OK", b"ok"));
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        let got = HttpClient::new().unwrap().fetch(&Request {
            method: Method::Post,
            url: &url,
            payload: Some(("application/json", b"{\"q\":1}")),
            user_agent: "sabigoku-test",
            extra_headers: &[("Referer", "https://ref.example/")],
            accept: Accept::OkOnly,
        });
        assert_eq!(got.unwrap(), b"ok");
        let seen = handle.join().unwrap();
        assert!(seen.starts_with("POST / HTTP/1.1\r\n"), "{seen}");
        assert!(seen.contains("user-agent: sabigoku-test"), "{seen}");
        assert!(seen.contains("content-type: application/json"), "{seen}");
        assert!(seen.contains("referer: https://ref.example/"), "{seen}");
        assert!(seen.ends_with("{\"q\":1}"), "{seen}");
    }
}
