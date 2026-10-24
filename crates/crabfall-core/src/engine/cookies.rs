//! Cookies de un VU (ADR-014): guarda las que manda el servidor y las reenvía, con las
//! reglas de un navegador (dominio, path, vencimiento, `Secure`).

use std::sync::Mutex;

use cookie_store::{CookieStore, RawCookie};
use hyper::HeaderMap;
use url::Url;

/// Un jar por VU. Apagado (`cookies: false`), no guarda ni manda nada.
pub struct Jar(Option<Mutex<CookieStore>>);

impl Jar {
    pub fn new(enabled: bool) -> Self {
        Self(enabled.then(|| Mutex::new(CookieStore::new())))
    }

    /// `nombre=valor; ...` para un request a `url`, o `None` si no hay ninguna.
    pub fn header(&self, url: &Url) -> Option<String> {
        let store = self.0.as_ref()?.lock().unwrap();
        let pairs: Vec<String> = store
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    /// Guarda los `set-cookie` de una respuesta a `url`. Los inválidos se ignoran, como en
    /// un navegador.
    pub fn store(&self, url: &Url, headers: &HeaderMap) {
        let Some(store) = &self.0 else { return };
        let cookies: Vec<RawCookie<'static>> = headers
            .get_all(hyper::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| RawCookie::parse(v.to_string()).ok())
            .collect();
        if !cookies.is_empty() {
            store
                .lock()
                .unwrap()
                .store_response_cookies(cookies.into_iter(), url);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(hyper::header::SET_COOKIE, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn keeps_cookies_by_path_and_expiry() {
        let jar = Jar::new(true);
        let login = Url::parse("http://127.0.0.1:3000/auth/login").unwrap();
        jar.store(
            &login,
            &headers(&[
                "sid=abc; Path=/; HttpOnly",
                "pref=1; Path=/admin",
                "old=x; Path=/; Max-Age=0",
            ]),
        );
        let api = Url::parse("http://127.0.0.1:3000/orders").unwrap();
        assert_eq!(jar.header(&api).as_deref(), Some("sid=abc"));
        let admin = Url::parse("http://127.0.0.1:3000/admin/users").unwrap();
        let both = jar.header(&admin).unwrap();
        assert!(
            both.contains("sid=abc") && both.contains("pref=1"),
            "{both}"
        );
    }

    #[test]
    fn secure_cookies_only_go_over_https() {
        let jar = Jar::new(true);
        let https = Url::parse("https://api.example.com/login").unwrap();
        jar.store(&https, &headers(&["sid=abc; Secure; Path=/"]));
        assert_eq!(jar.header(&https).as_deref(), Some("sid=abc"));
        let http = Url::parse("http://api.example.com/").unwrap();
        assert_eq!(jar.header(&http), None);
    }

    #[test]
    fn disabled_jar_ignores_everything() {
        let jar = Jar::new(false);
        let url = Url::parse("http://127.0.0.1/").unwrap();
        jar.store(&url, &headers(&["sid=abc"]));
        assert_eq!(jar.header(&url), None);
    }
}
