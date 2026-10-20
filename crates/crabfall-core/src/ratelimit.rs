//! Lo que un servidor dice de su rate limit en una respuesta 429: `Retry-After` y la
//! política declarada (headers `RateLimit-Policy` de los drafts de la IETF, o los
//! `X-RateLimit-*` de siempre).

use hyper::HeaderMap;
use serde::Serialize;

/// Lo que trajo una respuesta 429.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sample {
    /// `Retry-After` en segundos (solo la forma numérica; una fecha HTTP se ignora).
    pub retry_after_s: Option<u64>,
    /// Requests permitidas por ventana.
    pub quota: Option<u64>,
    /// Duración de la ventana, en segundos.
    pub window_s: Option<u64>,
}

impl Sample {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        let retry_after_s = get("retry-after").and_then(|v| v.trim().parse().ok());
        // draft-8: `"auth"; q=30; w=60`. drafts 6 y 7: `30;w=60`.
        let (mut quota, mut window_s) = get("ratelimit-policy")
            .and_then(|p| p.split(',').next())
            .map(parse_policy)
            .unwrap_or_default();
        // Sin política: la cuota de `RateLimit-Limit` o `X-RateLimit-Limit` (draft 6, legacy).
        if quota.is_none() {
            quota = get("ratelimit-limit")
                .or_else(|| get("x-ratelimit-limit"))
                .and_then(|v| v.split([',', ';']).next())
                .and_then(|v| v.trim().parse().ok());
        }
        if window_s.is_none() {
            window_s = get("x-ratelimit-window").and_then(|v| v.trim().parse().ok());
        }
        Self {
            retry_after_s,
            quota,
            window_s,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `"auth"; q=30; w=60` o `30;w=60` → (30, 60).
fn parse_policy(policy: &str) -> (Option<u64>, Option<u64>) {
    let mut quota = None;
    let mut window = None;
    for (i, part) in policy.split(';').map(str::trim).enumerate() {
        if let Some(q) = part.strip_prefix("q=") {
            quota = q.parse().ok();
        } else if let Some(w) = part.strip_prefix("w=") {
            window = w.parse().ok();
        } else if i == 0 {
            quota = part.parse().ok();
        }
    }
    (quota, window)
}

/// Lo que dijeron los 429 de un paso, para los resultados.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    /// Respuestas 429.
    pub responses: u64,
    /// `Retry-After` más chico y más grande, en segundos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_min_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_max_s: Option<u64>,
    /// La política que declaró el servidor (la de la primera respuesta que la trajo).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_s: Option<u64>,
}

impl Summary {
    pub fn add(&mut self, s: &Sample) {
        self.responses += 1;
        if let Some(r) = s.retry_after_s {
            self.retry_after_min_s = Some(self.retry_after_min_s.map_or(r, |m| m.min(r)));
            self.retry_after_max_s = Some(self.retry_after_max_s.map_or(r, |m| m.max(r)));
        }
        if self.quota.is_none() && s.quota.is_some() {
            self.quota = s.quota;
            self.window_s = s.window_s;
        }
    }

    /// `30 requests cada 60 s`, si el servidor la declaró.
    pub fn policy(&self) -> Option<String> {
        match (self.quota, self.window_s) {
            (Some(q), Some(w)) => Some(crate::tr!(
                "{q} requests every {w} s",
                "{q} requests cada {w} s"
            )),
            (Some(q), None) => Some(crate::tr!(
                "{q} requests per window",
                "{q} requests por ventana"
            )),
            _ => None,
        }
    }

    /// `Retry-After 23 s` o `Retry-After de 1 a 23 s`.
    pub fn retry_after(&self) -> Option<String> {
        match (self.retry_after_min_s, self.retry_after_max_s) {
            (Some(a), Some(b)) if a == b => Some(format!("Retry-After {a} s")),
            (Some(a), Some(b)) => Some(crate::tr!(
                "Retry-After from {a} to {b} s",
                "Retry-After de {a} a {b} s"
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                hyper::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn reads_draft_8_like_express_rate_limit() {
        let s = Sample::from_headers(&headers(&[
            ("ratelimit-policy", "\"auth\"; q=30; w=60; pk=:NGJl:"),
            ("ratelimit", "\"auth\"; r=0; t=23"),
            ("retry-after", "23"),
        ]));
        assert_eq!(
            s,
            Sample {
                retry_after_s: Some(23),
                quota: Some(30),
                window_s: Some(60)
            }
        );
    }

    #[test]
    fn reads_older_drafts_and_legacy_headers() {
        let draft7 = Sample::from_headers(&headers(&[("ratelimit-policy", "100;w=3600")]));
        assert_eq!((draft7.quota, draft7.window_s), (Some(100), Some(3600)));
        let legacy = Sample::from_headers(&headers(&[
            ("x-ratelimit-limit", "50"),
            ("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT"),
        ]));
        assert_eq!(legacy.quota, Some(50));
        assert_eq!(legacy.retry_after_s, None);
        assert!(Sample::from_headers(&HeaderMap::new()).is_empty());
    }

    #[test]
    fn summary_keeps_range_and_first_policy() {
        let mut sum = Summary::default();
        for (r, q) in [(Some(5), Some(30)), (Some(23), Some(99)), (None, None)] {
            sum.add(&Sample {
                retry_after_s: r,
                quota: q,
                window_s: q.map(|_| 60),
            });
        }
        assert_eq!(sum.responses, 3);
        assert_eq!(
            sum.retry_after().as_deref(),
            Some("Retry-After de 5 a 23 s")
        );
        assert_eq!(sum.policy().as_deref(), Some("30 requests cada 60 s"));
    }
}
