//! Unidades del escenario: duraciones (`500ms`, `30s`, `2m`, `1h`) y tasas (`50/s`, `3000/m`).

use std::{fmt, str::FromStr, time::Duration};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Duración escrita como número entero + unidad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumanDuration(pub Duration);

impl FromStr for HumanDuration {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid =
            || format!("duración inválida `{s}`: usá un entero y una unidad (500ms, 30s, 2m, 1h)");
        let (number, unit) = split_number(s.trim()).ok_or_else(invalid)?;
        let duration = match unit {
            "ms" => Duration::from_millis(number),
            "s" => Duration::from_secs(number),
            "m" => Duration::from_secs(number * 60),
            "h" => Duration::from_secs(number * 3600),
            _ => return Err(invalid()),
        };
        Ok(Self(duration))
    }
}

impl fmt::Display for HumanDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = self.0.as_millis();
        match ms {
            0 => write!(f, "0s"),
            _ if ms.is_multiple_of(3_600_000) => write!(f, "{}h", ms / 3_600_000),
            _ if ms.is_multiple_of(60_000) => write!(f, "{}m", ms / 60_000),
            _ if ms.is_multiple_of(1000) => write!(f, "{}s", ms / 1000),
            _ => write!(f, "{ms}ms"),
        }
    }
}

/// Tasa escrita como `N/s` o `N/m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub count: u64,
    pub per: Duration,
}

impl Rate {
    pub fn per_second(&self) -> f64 {
        self.count as f64 / self.per.as_secs_f64()
    }
}

impl FromStr for Rate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("tasa inválida `{s}`: usá un entero por unidad (50/s, 3000/m)");
        let (count, unit) = s.trim().split_once('/').ok_or_else(invalid)?;
        let count = count.trim().parse().map_err(|_| invalid())?;
        let per = match unit.trim() {
            "s" => Duration::from_secs(1),
            "m" => Duration::from_secs(60),
            _ => return Err(invalid()),
        };
        Ok(Self { count, per })
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = if self.per.as_secs() == 60 { "m" } else { "s" };
        write!(f, "{}/{unit}", self.count)
    }
}

fn split_number(s: &str) -> Option<(u64, &str)> {
    let digits = s.find(|c: char| !c.is_ascii_digit())?;
    let number = s[..digits].parse().ok()?;
    Some((number, &s[digits..]))
}

macro_rules! serde_via_str {
    ($ty:ty) => {
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                s.parse().map_err(de::Error::custom)
            }
        }

        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }
    };
}

serde_via_str!(HumanDuration);
serde_via_str!(Rate);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(
            "500ms".parse::<HumanDuration>().unwrap().0,
            Duration::from_millis(500)
        );
        assert_eq!(
            "30s".parse::<HumanDuration>().unwrap().0,
            Duration::from_secs(30)
        );
        assert_eq!(
            "2m".parse::<HumanDuration>().unwrap().0,
            Duration::from_secs(120)
        );
        assert_eq!(
            "1h".parse::<HumanDuration>().unwrap().0,
            Duration::from_secs(3600)
        );
        assert!("30".parse::<HumanDuration>().is_err());
        assert!("1.5s".parse::<HumanDuration>().is_err());
        assert!("s".parse::<HumanDuration>().is_err());
    }

    #[test]
    fn displays_durations_in_largest_exact_unit() {
        assert_eq!(
            HumanDuration(Duration::from_millis(90_000)).to_string(),
            "90s"
        );
        assert_eq!(HumanDuration(Duration::from_secs(120)).to_string(), "2m");
        assert_eq!(
            HumanDuration(Duration::from_millis(1500)).to_string(),
            "1500ms"
        );
    }

    #[test]
    fn parses_rates() {
        assert_eq!("50/s".parse::<Rate>().unwrap().per_second(), 50.0);
        assert_eq!("3000/m".parse::<Rate>().unwrap().per_second(), 50.0);
        assert!("50".parse::<Rate>().is_err());
        assert!("50/h".parse::<Rate>().is_err());
    }
}
