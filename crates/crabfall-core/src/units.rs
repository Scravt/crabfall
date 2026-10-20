//! Unidades del escenario: duraciones (`500ms`, `30s`, `2m`, `1h`), tasas (`50/s`, `3000/m`) y pausas (`2s`, `1s-3s`).

use std::{fmt, str::FromStr, time::Duration};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Duración escrita como número entero + unidad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumanDuration(pub Duration);

impl FromStr for HumanDuration {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            crate::tr!(
                "invalid duration `{s}`: use an integer and a unit (500ms, 30s, 2m, 1h)",
                "duración inválida `{s}`: usá un entero y una unidad (500ms, 30s, 2m, 1h)"
            )
        };
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
        let invalid = || {
            crate::tr!(
                "invalid rate `{s}`: use an integer per unit (50/s, 3000/m)",
                "tasa inválida `{s}`: usá un entero por unidad (50/s, 3000/m)"
            )
        };
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

/// Pausa después de un paso: fija (`2s`) o al azar en un rango (`1s-3s`). `0s` no pausa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Think {
    pub min: Duration,
    pub max: Duration,
}

impl Think {
    pub fn is_zero(&self) -> bool {
        self.max.is_zero()
    }

    /// Una pausa: `min` si es fija, o uniforme entre `min` y `max`.
    pub fn sample(&self) -> Duration {
        if self.min == self.max {
            return self.min;
        }
        let (min, max) = (self.min.as_micros() as u64, self.max.as_micros() as u64);
        Duration::from_micros(fastrand::u64(min..=max))
    }
}

impl FromStr for Think {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            crate::tr!(
                "invalid pause `{s}`: use a duration (2s) or a range (1s-3s, 500ms-2s)",
                "pausa inválida `{s}`: usá una duración (2s) o un rango (1s-3s, 500ms-2s)"
            )
        };
        let (min, max) = match s.trim().split_once('-') {
            Some((a, b)) => (
                a.parse::<HumanDuration>().map_err(|_| invalid())?,
                b.parse::<HumanDuration>().map_err(|_| invalid())?,
            ),
            None => {
                let d = s.parse::<HumanDuration>().map_err(|_| invalid())?;
                (d, d)
            }
        };
        if min.0 > max.0 {
            return Err(crate::tr!(
                "invalid pause `{s}`: the minimum is greater than the maximum",
                "pausa inválida `{s}`: el mínimo es mayor que el máximo"
            ));
        }
        Ok(Self {
            min: min.0,
            max: max.0,
        })
    }
}

impl fmt::Display for Think {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min == self.max {
            write!(f, "{}", HumanDuration(self.min))
        } else {
            write!(f, "{}-{}", HumanDuration(self.min), HumanDuration(self.max))
        }
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
serde_via_str!(Think);

impl schemars::JsonSchema for HumanDuration {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Duration".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[0-9]+(ms|s|m|h)$",
            "description": "An integer and a unit: 500ms, 30s, 2m, 1h.",
        })
    }
}

impl schemars::JsonSchema for Rate {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Rate".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[0-9]+/(s|m)$",
            "description": "An integer per second or per minute: 50/s, 3000/m.",
        })
    }
}

impl schemars::JsonSchema for Think {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Think".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[0-9]+(ms|s|m|h)(-[0-9]+(ms|s|m|h))?$",
            "description": "A fixed duration (2s) or a random range (1s-3s). 0s: no pause.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_think_times() {
        let fixed: Think = "2s".parse().unwrap();
        assert_eq!(
            (fixed.min, fixed.max),
            (Duration::from_secs(2), Duration::from_secs(2))
        );
        assert_eq!(fixed.sample(), Duration::from_secs(2));
        assert_eq!(fixed.to_string(), "2s");

        let range: Think = "500ms-2s".parse().unwrap();
        assert_eq!(range.to_string(), "500ms-2s");
        for _ in 0..100 {
            let d = range.sample();
            assert!(d >= Duration::from_millis(500) && d <= Duration::from_secs(2));
        }
        assert!("0s".parse::<Think>().unwrap().is_zero());
        assert!("3s-1s".parse::<Think>().unwrap_err().contains("mayor"));
        assert!("1s-".parse::<Think>().is_err());
        assert!("2".parse::<Think>().is_err());
    }

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
