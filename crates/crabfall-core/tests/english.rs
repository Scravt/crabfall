//! Messages in English (ADR-016). The workspace runs tests with `CRABFALL_LANG=es`, so this
//! binary chooses English explicitly; it lives apart because the language is global.

use crabfall_core::{
    i18n::{self, Lang},
    scenario::Threshold,
    validate::parse_scenario,
};

const OK: &str = "version: 1
name: s
flows:
  main:
    steps:
      - name: a
        request: { method: GET, url: /x }
load: { executor: constant_rate, rate: 1/s, duration: 1s, max_vus: 1 }
";

#[test]
fn validation_parsing_and_summaries_speak_english() {
    i18n::set(Lang::En);

    let report = parse_scenario(&OK.replace("url: /x", "url: x")).unwrap_err();
    assert_eq!(
        report.errors[0].message,
        "must start with `/` (relative to base_url) or with http:// or https://"
    );

    let rate = parse_scenario(&OK.replace("rate: 1/s", "rate: 1")).unwrap_err();
    assert!(
        rate.errors[0].message.contains("invalid rate `1`"),
        "{}",
        rate.errors[0].message
    );

    let t = "http.latency.p95~300ms".parse::<Threshold>().unwrap_err();
    assert!(t.starts_with("invalid threshold"), "{t}");

    let s = parse_scenario(OK).unwrap();
    assert_eq!(
        s.load.plan().describe(),
        "constant_rate 1/s for 1s, max_vus 1"
    );
}
