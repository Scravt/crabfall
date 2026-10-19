//! Thresholds: criterios de aprobación de una corrida (ADR-002).

use serde::Serialize;
use serde_json::Value;

use crate::{scenario::Threshold, units::HumanDuration};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Metric {
    /// Latencia en ms: percentil (0..100), promedio o máximo.
    Latency(Stat),
    /// Porcentaje de requests sin respuesta o con status >= 400.
    ErrorRate,
    /// Requests por segundo.
    Rps,
    /// Porcentaje de checks que pasaron.
    ChecksPassRate,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stat {
    Quantile(f64),
    Avg,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Lt,
    Lte,
    Gt,
    Gte,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    All,
    Step(String),
    Tag(String),
}

#[derive(Debug, Clone)]
pub struct ThresholdPlan {
    /// `http.latency.p95{step=create_order} < 300ms`
    pub label: String,
    pub metric: Metric,
    pub filter: Filter,
    pub op: Op,
    /// En la unidad de la métrica: ms, % o requests/s.
    pub limit: f64,
}

impl ThresholdPlan {
    pub fn parse(t: &Threshold) -> Result<Self, String> {
        let metric = parse_metric(&t.metric)?;
        let filter = match (&t.step, &t.tag) {
            (None, None) => Filter::All,
            (Some(s), None) => Filter::Step(s.clone()),
            (None, Some(tag)) => Filter::Tag(tag.clone()),
            (Some(_), Some(_)) => return Err("usá `step` o `tag`, no los dos".into()),
        };
        let ops = [
            (Op::Lt, &t.lt, "<"),
            (Op::Lte, &t.lte, "<="),
            (Op::Gt, &t.gt, ">"),
            (Op::Gte, &t.gte, ">="),
        ];
        let given: Vec<_> = ops.iter().filter(|(_, v, _)| v.is_some()).collect();
        let [(op, Some(value), symbol)] = given.as_slice() else {
            return Err("usá exactamente uno de: lt, lte, gt, gte".into());
        };
        let limit = parse_limit(metric, value)?;
        let scope = match &filter {
            Filter::All => String::new(),
            Filter::Step(s) => format!("{{step={s}}}"),
            Filter::Tag(t) => format!("{{tag={t}}}"),
        };
        let shown = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        Ok(Self {
            label: format!("{}{scope} {symbol} {shown}", t.metric),
            metric,
            filter,
            op: *op,
            limit,
        })
    }

    pub fn unit(&self) -> &'static str {
        match self.metric {
            Metric::Latency(_) => "ms",
            Metric::ErrorRate | Metric::ChecksPassRate => "%",
            Metric::Rps => "req/s",
        }
    }

    pub fn passes(&self, actual: f64) -> bool {
        match self.op {
            Op::Lt => actual < self.limit,
            Op::Lte => actual <= self.limit,
            Op::Gt => actual > self.limit,
            Op::Gte => actual >= self.limit,
        }
    }
}

fn parse_metric(name: &str) -> Result<Metric, String> {
    let bad = || {
        format!(
            "métrica `{name}` desconocida (usá http.latency.p50…p99.9, http.latency.avg, http.latency.max, \
             http.error_rate, http.rps o checks.pass_rate)"
        )
    };
    match name {
        "http.error_rate" => return Ok(Metric::ErrorRate),
        "http.rps" => return Ok(Metric::Rps),
        "checks.pass_rate" => return Ok(Metric::ChecksPassRate),
        "http.latency.avg" => return Ok(Metric::Latency(Stat::Avg)),
        "http.latency.max" => return Ok(Metric::Latency(Stat::Max)),
        _ => {}
    }
    let p: f64 = name
        .strip_prefix("http.latency.p")
        .and_then(|p| p.parse().ok())
        .ok_or_else(bad)?;
    if !(0.0 < p && p < 100.0) {
        return Err(bad());
    }
    Ok(Metric::Latency(Stat::Quantile(p)))
}

fn parse_limit(metric: Metric, value: &Value) -> Result<f64, String> {
    match metric {
        Metric::Latency(_) => match value {
            Value::String(s) => s
                .parse::<HumanDuration>()
                .map(|d| d.0.as_secs_f64() * 1000.0)
                .map_err(|e| format!("{e} (las latencias se comparan contra una duración)")),
            Value::Number(n) => Ok(n.as_f64().unwrap_or_default()),
            _ => Err("la latencia se compara contra una duración, por ejemplo `300ms`".into()),
        },
        Metric::ErrorRate | Metric::ChecksPassRate => value
            .as_str()
            .and_then(|s| s.trim().strip_suffix('%'))
            .and_then(|n| n.trim().parse::<f64>().ok())
            .filter(|p| (0.0..=100.0).contains(p))
            .ok_or_else(|| "usá un porcentaje entre 0% y 100%, por ejemplo `1%`".into()),
        Metric::Rps => value
            .as_f64()
            .ok_or_else(|| "http.rps se compara contra un número".into()),
    }
}

/// `http.latency.p95<300ms`, `http.latency.p99{step=login}<=1s`, `http.rps>=40`: la forma
/// de `--threshold` (ADR-008). Valida igual que el bloque `thresholds` del YAML.
impl std::str::FromStr for Threshold {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || {
            format!(
                "threshold `{s}` inválido: usá métrica[{{step=x}}] seguido de <, <=, > o >= y el límite \
                 (http.latency.p95<300ms, http.error_rate<1%, http.rps>=40)"
            )
        };
        let at = s.find(['<', '>']).ok_or_else(bad)?;
        let (lhs, rest) = s.split_at(at);
        let (symbol, value) = match rest.strip_prefix("<=").or(rest.strip_prefix(">=")) {
            Some(v) => (&rest[..2], v),
            None => (&rest[..1], &rest[1..]),
        };
        let (metric, step, tag) = match lhs.trim().split_once('{') {
            None => (lhs.trim(), None, None),
            Some((metric, filter)) => {
                let filter = filter.strip_suffix('}').ok_or_else(bad)?;
                match filter.split_once('=') {
                    Some(("step", v)) => (metric, Some(v.trim().to_string()), None),
                    Some(("tag", v)) => (metric, None, Some(v.trim().to_string())),
                    _ => return Err(bad()),
                }
            }
        };
        let value = value.trim();
        if metric.is_empty() || value.is_empty() {
            return Err(bad());
        }
        let value = value
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(value.to_string()), Value::Number);
        let mut t = Threshold {
            metric: metric.to_string(),
            step,
            tag,
            ..Default::default()
        };
        *match symbol {
            "<" => &mut t.lt,
            "<=" => &mut t.lte,
            ">" => &mut t.gt,
            _ => &mut t.gte,
        } = Some(value);
        ThresholdPlan::parse(&t)?;
        Ok(t)
    }
}

/// Un threshold como valor de `--threshold`, sin espacios: `http.latency.p95{step=a}<300ms`.
pub fn to_flag(t: &Threshold) -> String {
    let scope = match (&t.step, &t.tag) {
        (Some(s), _) => format!("{{step={s}}}"),
        (_, Some(tag)) => format!("{{tag={tag}}}"),
        _ => String::new(),
    };
    let (symbol, value) = [("<", &t.lt), ("<=", &t.lte), (">", &t.gt), (">=", &t.gte)]
        .into_iter()
        .find_map(|(s, v)| v.as_ref().map(|v| (s, v)))
        .unwrap_or(("", &Value::Null));
    let value = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    format!("{}{scope}{symbol}{value}", t.metric)
}

/// Resultado de evaluar un threshold.
#[derive(Debug, Clone, Serialize)]
pub struct ThresholdResult {
    pub threshold: String,
    pub op: Op,
    pub limit: f64,
    /// Valor medido; `None` si no hubo datos.
    pub actual: Option<f64>,
    pub unit: &'static str,
    pub passed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<ThresholdPlan, String> {
        ThresholdPlan::parse(&serde_saphyr::from_str::<Threshold>(yaml).unwrap())
    }

    #[test]
    fn parses_metrics_and_limits() {
        let t = parse("{ metric: http.latency.p99.9, step: buy, lt: 1s }").unwrap();
        assert_eq!(t.metric, Metric::Latency(Stat::Quantile(99.9)));
        assert_eq!(
            (t.limit, t.label.as_str()),
            (1000.0, "http.latency.p99.9{step=buy} < 1s")
        );
        assert!(t.passes(999.0) && !t.passes(1000.0));

        let t = parse("{ metric: http.error_rate, lte: \"0.5%\" }").unwrap();
        assert_eq!((t.metric, t.limit), (Metric::ErrorRate, 0.5));
        assert_eq!(parse("{ metric: http.rps, gte: 40 }").unwrap().limit, 40.0);
    }

    #[test]
    fn parses_the_flag_form() {
        let t: Threshold = "http.latency.p99{step=login}<=1s".parse().unwrap();
        assert_eq!(
            (t.metric.as_str(), t.step.as_deref()),
            ("http.latency.p99", Some("login"))
        );
        assert_eq!(t.lte, Some(Value::String("1s".into())));
        assert_eq!(to_flag(&t), "http.latency.p99{step=login}<=1s");

        let t: Threshold = "http.rps >= 40".parse().unwrap();
        assert_eq!(t.gte, Some(serde_json::json!(40.0)));
        let t: Threshold = "http.error_rate{tag=write}<1%".parse().unwrap();
        assert_eq!(to_flag(&t), "http.error_rate{tag=write}<1%");

        for bad in [
            "p95<1s",
            "http.latency.p95",
            "http.latency.p95<",
            "http.rps{x=y}>1",
            "http.error_rate<1",
        ] {
            assert!(bad.parse::<Threshold>().is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_bad_thresholds() {
        assert!(
            parse("{ metric: http.latency.p100, lt: 1s }")
                .unwrap_err()
                .contains("desconocida")
        );
        assert!(
            parse("{ metric: http.latency.p95, lt: 1s, gt: 2s }")
                .unwrap_err()
                .contains("exactamente uno")
        );
        assert!(
            parse("{ metric: http.error_rate, lt: 1 }")
                .unwrap_err()
                .contains("porcentaje")
        );
        assert!(parse("{ metric: http.latency.p95, lt: rápido }").is_err());
        assert!(
            parse("{ metric: http.rps, step: a, tag: b, gt: 1 }")
                .unwrap_err()
                .contains("no los dos")
        );
    }
}
