//! Perfil de carga resuelto y su calendario de iteraciones (ADR-003).

use std::time::Duration;

use serde_json::{Value, json};

use crate::units::HumanDuration;

/// Un tramo de tasa lineal: de `from` a `to` iteraciones/s durante `duration`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub from: f64,
    pub to: f64,
    pub duration: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoadPlan {
    /// Modelo abierto: las iteraciones arrancan según el calendario, sin esperar respuestas.
    /// `constant_rate` es un solo tramo plano.
    Open {
        ramping: bool,
        segments: Vec<Segment>,
        max_vus: u32,
        graceful_stop: Duration,
    },
    /// Modelo cerrado: `vus` usuarios en loop; la tasa depende de la latencia.
    Closed {
        vus: u32,
        duration: Duration,
        graceful_stop: Duration,
    },
}

impl LoadPlan {
    pub fn executor(&self) -> &'static str {
        match self {
            Self::Open { ramping: false, .. } => "constant_rate",
            Self::Open { ramping: true, .. } => "ramping_rate",
            Self::Closed { .. } => "constant_vus",
        }
    }

    pub fn duration(&self) -> Duration {
        match self {
            Self::Open { segments, .. } => segments.iter().map(|s| s.duration).sum(),
            Self::Closed { duration, .. } => *duration,
        }
    }

    pub fn graceful_stop(&self) -> Duration {
        match self {
            Self::Open { graceful_stop, .. } | Self::Closed { graceful_stop, .. } => *graceful_stop,
        }
    }

    /// Máximo de iteraciones en vuelo.
    pub fn concurrency(&self) -> u32 {
        match self {
            Self::Open { max_vus, .. } => *max_vus,
            Self::Closed { vus, .. } => *vus,
        }
    }

    /// Tasa máxima de iteraciones/s. `None` en el modelo cerrado: depende de la latencia.
    pub fn peak_rate(&self) -> Option<f64> {
        match self {
            Self::Open { segments, .. } => Some(
                segments
                    .iter()
                    .map(|s| s.from.max(s.to))
                    .fold(0.0, f64::max),
            ),
            Self::Closed { .. } => None,
        }
    }

    /// Iteraciones que planifica el calendario (modelo abierto).
    pub fn planned_iterations(&self) -> Option<u64> {
        match self {
            Self::Open { segments, .. } => Some(
                (segments
                    .iter()
                    .map(|s| (s.from + s.to) / 2.0 * s.duration.as_secs_f64())
                    .sum::<f64>()
                    + 1e-9)
                    .floor() as u64,
            ),
            Self::Closed { .. } => None,
        }
    }

    /// Una línea para humanos: `ramping_rate 0/s → 100/s (2m) → 0/s (30s), max_vus 200`.
    pub fn describe(&self) -> String {
        match self {
            Self::Open {
                ramping: false,
                segments,
                max_vus,
                ..
            } => crate::tr!(
                "constant_rate {} for {}, max_vus {max_vus}",
                "constant_rate {} durante {}, max_vus {max_vus}",
                rate(segments[0].from),
                HumanDuration(segments[0].duration)
            ),
            Self::Open {
                segments, max_vus, ..
            } => {
                let mut text = format!("ramping_rate {}", rate(segments[0].from));
                for s in segments {
                    text.push_str(&format!(
                        " → {} ({})",
                        rate(s.to),
                        HumanDuration(s.duration)
                    ));
                }
                format!("{text}, max_vus {max_vus}")
            }
            Self::Closed { vus, duration, .. } => {
                crate::tr!(
                    "constant_vus {vus} for {}",
                    "constant_vus {vus} durante {}",
                    HumanDuration(*duration)
                )
            }
        }
    }

    /// Para la firma de la aprobación y los resultados.
    pub fn canonical(&self) -> Value {
        let d = |d: Duration| HumanDuration(d).to_string();
        match self {
            Self::Open {
                ramping: false,
                segments,
                max_vus,
                graceful_stop,
            } => json!({
                "executor": self.executor(),
                "rate": rate(segments[0].from),
                "duration": d(segments[0].duration),
                "max_vus": max_vus,
                "graceful_stop": d(*graceful_stop),
            }),
            Self::Open {
                segments,
                max_vus,
                graceful_stop,
                ..
            } => json!({
                "executor": self.executor(),
                "start_rate": rate(segments[0].from),
                "stages": segments.iter().map(|s| json!({ "target": rate(s.to), "duration": d(s.duration) })).collect::<Vec<_>>(),
                "max_vus": max_vus,
                "graceful_stop": d(*graceful_stop),
            }),
            Self::Closed {
                vus,
                duration,
                graceful_stop,
            } => json!({
                "executor": self.executor(),
                "vus": vus,
                "duration": d(*duration),
                "graceful_stop": d(*graceful_stop),
            }),
        }
    }

    /// Instantes planificados de cada iteración, desde el inicio (modelo abierto).
    pub fn schedule(&self) -> Schedule {
        let (segments, total) = match self {
            Self::Open { segments, .. } => {
                (segments.clone(), self.planned_iterations().unwrap_or(0))
            }
            Self::Closed { .. } => (vec![], 0),
        };
        Schedule {
            segments,
            seg: 0,
            seg_start: 0.0,
            seg_count: 0.0,
            next: 0,
            total,
        }
    }
}

fn rate(r: f64) -> String {
    if r.fract() == 0.0 {
        format!("{r:.0}/s")
    } else {
        format!("{r:.2}/s")
    }
}

/// La iteración `i` arranca cuando la integral de la tasa llega a `i`. En un tramo lineal
/// eso es resolver `from·t + (to-from)/(2d)·t² = k`: exacto, sin acumular error.
pub struct Schedule {
    segments: Vec<Segment>,
    seg: usize,
    seg_start: f64,
    seg_count: f64,
    next: u64,
    total: u64,
}

impl Iterator for Schedule {
    type Item = Duration;

    fn next(&mut self) -> Option<Duration> {
        if self.next >= self.total {
            return None;
        }
        let i = self.next as f64;
        loop {
            let s = self.segments[self.seg];
            let d = s.duration.as_secs_f64();
            let in_segment = (s.from + s.to) / 2.0 * d;
            let k = i - self.seg_count;
            if k < in_segment || self.seg + 1 == self.segments.len() {
                let a = (s.to - s.from) / (2.0 * d);
                let b = s.from;
                let t = if a.abs() < 1e-12 {
                    if b > 0.0 { k / b } else { 0.0 }
                } else {
                    (-b + (b * b + 4.0 * a * k).max(0.0).sqrt()) / (2.0 * a)
                };
                self.next += 1;
                return Some(Duration::from_secs_f64(self.seg_start + t.clamp(0.0, d)));
            }
            self.seg_start += d;
            self.seg_count += in_segment;
            self.seg += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(segments: &[(f64, f64, u64)]) -> LoadPlan {
        LoadPlan::Open {
            ramping: true,
            segments: segments
                .iter()
                .map(|&(from, to, s)| Segment {
                    from,
                    to,
                    duration: Duration::from_secs(s),
                })
                .collect(),
            max_vus: 10,
            graceful_stop: Duration::from_secs(1),
        }
    }

    #[test]
    fn constant_rate_is_evenly_spaced() {
        let plan = open(&[(10.0, 10.0, 2)]);
        let t: Vec<f64> = plan.schedule().map(|d| d.as_secs_f64()).collect();
        assert_eq!(t.len(), 20);
        assert!((t[1] - 0.1).abs() < 1e-9 && (t[19] - 1.9).abs() < 1e-9);
    }

    #[test]
    fn ramp_follows_the_integral() {
        // 0 → 100/s en 10 s: 500 iteraciones; la mitad (250) llega a t = 10/√2 ≈ 7.07 s.
        let plan = open(&[(0.0, 100.0, 10), (100.0, 100.0, 5), (100.0, 0.0, 10)]);
        assert_eq!(plan.planned_iterations(), Some(500 + 500 + 500));
        let t: Vec<f64> = plan.schedule().map(|d| d.as_secs_f64()).collect();
        assert!((t[250] - 10.0 / 2f64.sqrt()).abs() < 1e-6, "{}", t[250]);
        assert!((t[500] - 10.0).abs() < 1e-6);
        assert!((t[750] - 12.5).abs() < 1e-6);
        assert!(t.windows(2).all(|w| w[0] <= w[1]));
        assert!(*t.last().unwrap() <= 25.0);
        assert_eq!(plan.peak_rate(), Some(100.0));
        assert_eq!(plan.duration(), Duration::from_secs(25));
    }

    #[test]
    fn describes_profiles() {
        assert_eq!(
            open(&[(0.0, 100.0, 120), (100.0, 0.0, 30)]).describe(),
            "ramping_rate 0/s → 100/s (2m) → 0/s (30s), max_vus 10"
        );
        let closed = LoadPlan::Closed {
            vus: 20,
            duration: Duration::from_secs(60),
            graceful_stop: Duration::ZERO,
        };
        assert_eq!(closed.describe(), "constant_vus 20 durante 1m");
        assert_eq!(closed.peak_rate(), None);
    }
}
