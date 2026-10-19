//! `crabfall compare`: qué mejoró y qué empeoró entre dos corridas (ADR-006).

use crabfall_core::{
    compare::{Change, Comparison, Delta, FindingState, Presence, Run, Verdict, compare, ms},
    diagnose::Severity,
    project::Project,
};
use serde_json::json;

use crate::{JSON_FORMAT, display, exit, runs};

pub fn cmd_compare(base: Option<&str>, new: Option<&str>, as_json: bool) -> u8 {
    let Some(project) = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd))
    else {
        eprintln!("✗ no encontré .crabfall/ en este directorio ni en los superiores");
        return exit::INVALID;
    };
    let loaded = (|| {
        let new_file = runs::file(&project, new)?;
        let new_doc = runs::read(&new_file)?;
        let base_file = match base {
            Some(id) => runs::file(&project, Some(id))?,
            None => runs::previous(
                &project,
                new_doc["run_id"].as_str().unwrap_or_default(),
                new_doc["scenario"].as_str().unwrap_or_default(),
                new_doc["environment"].as_str().unwrap_or_default(),
            )?,
        };
        let base_doc = runs::read(&base_file)?;
        let parse = |doc, file: &std::path::Path| {
            serde_json::from_value::<Run>(doc)
                .map_err(|e| format!("{} no tiene el formato esperado: {e}", display(file)))
        };
        Ok::<_, String>((
            (base_file.clone(), parse(base_doc, &base_file)?),
            (new_file.clone(), parse(new_doc, &new_file)?),
        ))
    })();
    let ((base_file, base_run), (new_file, new_run)) = match loaded {
        Ok(runs) => runs,
        Err(e) => {
            eprintln!("✗ {e}");
            return exit::INVALID;
        }
    };
    let c = match compare(&base_run, &new_run) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("✗ {e}");
            return exit::INVALID;
        }
    };

    if as_json {
        let mut out = json!({
            "format": JSON_FORMAT,
            "base_file": display(&base_file),
            "new_file": display(&new_file),
        });
        let serde_json::Value::Object(fields) = serde_json::to_value(&c).unwrap() else {
            unreachable!()
        };
        out.as_object_mut().unwrap().extend(fields);
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        print_comparison(&c);
    }
    if c.regressions.is_empty() {
        exit::OK
    } else {
        exit::FAILED
    }
}

fn print_comparison(c: &Comparison) {
    let scenario = if c.base.scenario == c.new.scenario {
        c.new.scenario.clone()
    } else {
        format!("{} → {}", c.base.scenario, c.new.scenario)
    };
    println!(
        "{} → {} · {scenario} · {}",
        c.base.run_id, c.new.run_id, c.new.environment
    );
    for w in &c.warnings {
        println!("AVISO: {w}");
    }
    let verdict = match c.verdict {
        Verdict::Better => "mejoró",
        Verdict::Worse => "empeoró",
        Verdict::Same => "sin cambios significativos",
        Verdict::Mixed => "mixto: hay mejoras y regresiones",
    };
    println!("Veredicto: {verdict}");
    if c.same_load {
        println!(
            "  misma carga: {} segundos emparejados para separar cambio de ruido",
            c.paired_windows
        );
    }
    print_list("Regresiones:", "✗", &c.regressions);
    print_list("Mejoras:", "✓", &c.improvements);

    println!();
    let width = c
        .steps
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!(
        "{:<width$}  {:>15}  {:>17}  {:>27}  {:>17}  {:>13}  latencia",
        "paso", "requests", "p50", "p95", "p99", "errores"
    );
    for s in &c.steps {
        let change = match s.presence {
            Presence::OnlyBase => "solo en la base".to_string(),
            Presence::OnlyNew => "solo en la nueva".to_string(),
            Presence::Both => {
                let word = match s.change {
                    Change::Better => "mejoró",
                    Change::Worse => "empeoró",
                    Change::Same => "igual",
                    Change::Uncertain => "¿ruido?",
                };
                match s.sign_test {
                    Some(t) if s.change != Change::Same && t.windows > 0 => {
                        let agree = if s.p95_ms.diff().is_some_and(|d| d > 0.0) {
                            t.worse
                        } else {
                            t.better
                        };
                        format!("{word} ({agree} de {} s)", t.windows)
                    }
                    _ => word.to_string(),
                }
            }
        };
        let p95 = match s.p95_ms.change_pct {
            Some(p) => format!("{} ({p:+.1}%)", pair(s.p95_ms, ms)),
            None => pair(s.p95_ms, ms),
        };
        println!(
            "{:<width$}  {:>15}  {:>17}  {:>27}  {:>17}  {:>13}  {change}",
            s.name,
            pair(s.requests, |v| format!("{v:.0}")),
            pair(s.p50_ms, ms),
            p95,
            pair(s.p99_ms, ms),
            pair(s.error_pct, |v| format!("{v:.1}%")),
        );
    }
    println!("latencia en ms; el veredicto de cada paso sale del p95");

    if !c.findings.is_empty() {
        println!();
        println!("Hallazgos:");
        for f in &c.findings {
            let state = match f.state {
                FindingState::New => "nuevo   ",
                FindingState::Resolved => "resuelto",
                FindingState::Persists => "sigue   ",
            };
            let mark = match f.severity {
                Severity::High => "✗✗",
                Severity::Medium => "✗ ",
                Severity::Info => "· ",
            };
            let title = f.new_title.as_ref().or(f.base_title.as_ref()).unwrap();
            println!("  {state} {mark} {title}");
        }
    }

    let changed: Vec<_> = c
        .thresholds
        .iter()
        .filter(|t| t.base_passed != t.new_passed)
        .collect();
    if !changed.is_empty() {
        println!();
        println!("Thresholds que cambiaron:");
        for t in changed {
            let state = |p: Option<bool>| match p {
                Some(true) => "pasa",
                Some(false) => "falla",
                None => "no está",
            };
            println!(
                "  {}: {} → {}",
                t.threshold,
                state(t.base_passed),
                state(t.new_passed)
            );
        }
    }
}

fn print_list(title: &str, mark: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    println!("{title}");
    for line in lines {
        println!("  {mark} {line}");
    }
}

/// `base → nueva`, con `-` donde no hay dato.
fn pair(d: Delta, f: impl Fn(f64) -> String) -> String {
    let side = |v: Option<f64>| v.map_or("-".to_string(), &f);
    format!("{} → {}", side(d.base), side(d.new))
}
