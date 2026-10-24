//! Language of everything Crabfall prints (ADR-016). English by default; `--lang`, then
//! `CRABFALL_LANG`, then `language:` in `.crabfall/config.yaml` choose another one.
//!
//! Every message keeps both versions next to each other: `tr!("english {x}", "español {x}")`.

use std::{
    str::FromStr,
    sync::atomic::{AtomicU8, Ordering::Relaxed},
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Es,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
        }
    }
}

impl FromStr for Lang {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" | "english" | "inglés" | "ingles" => Ok(Lang::En),
            "es" | "spanish" | "español" | "espanol" => Ok(Lang::Es),
            other => Err(format!(
                "unknown language `{other}`: use `en` or `es` / idioma desconocido `{other}`: usá `en` o `es`"
            )),
        }
    }
}

/// 0: not chosen yet; 1: English; 2: Spanish.
static CHOSEN: AtomicU8 = AtomicU8::new(0);

fn encode(l: Lang) -> u8 {
    match l {
        Lang::En => 1,
        Lang::Es => 2,
    }
}

/// The language in use: the one chosen with `set`, else `CRABFALL_LANG`, else English.
pub fn lang() -> Lang {
    match CHOSEN.load(Relaxed) {
        1 => Lang::En,
        2 => Lang::Es,
        _ => from_env().unwrap_or_default(),
    }
}

pub fn is_es() -> bool {
    lang() == Lang::Es
}

/// Chooses the language explicitly (`--lang`).
pub fn set(l: Lang) {
    CHOSEN.store(encode(l), Relaxed);
}

/// `language:` from `config.yaml`: applies only if neither `--lang` nor `CRABFALL_LANG`
/// chose one.
pub fn set_from_config(l: Lang) {
    if from_env().is_none() {
        let _ = CHOSEN.compare_exchange(0, encode(l), Relaxed, Relaxed);
    }
}

fn from_env() -> Option<Lang> {
    std::env::var("CRABFALL_LANG").ok()?.parse().ok()
}

/// One of two fixed texts, for `&'static str` contexts.
pub fn pick(en: &'static str, es: &'static str) -> &'static str {
    if is_es() { es } else { en }
}

/// A message in the language in use: `tr!("english {x}", "español {x}", args...)`.
/// Both are `format!` strings, with the same arguments.
#[macro_export]
macro_rules! tr {
    ($en:literal, $es:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_es() {
            format!($es $(, $arg)*)
        } else {
            format!($en $(, $arg)*)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codes_and_names() {
        assert_eq!("EN".parse::<Lang>(), Ok(Lang::En));
        assert_eq!("español".parse::<Lang>(), Ok(Lang::Es));
        assert!("fr".parse::<Lang>().is_err());
        assert_eq!(Lang::Es.code(), "es");
    }
}
