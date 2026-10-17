//! `lt run`: ejecuta un escenario y guarda los resultados en `.loadtest/runs/<id>/`.

use std::{
    io::IsTerminal,
    path::Path,
    sync::{Arc, atomic::Ordering::Relaxed},
    time::{Duration, Instant},
};

use lt_core::{
    engine::{self, Progress, plan::Plan},
    metrics::{CheckResult, RunResult},
    project::Project,
    timer,
};
use serde_json::{Value, json};

use crate::{
    JSON_FORMAT, display, exit,
    prepare::{Prepared, prepare},
};

pub fn cmd_run(target: &crate::Target, as_json: bool) -> u8 {
    let Prepared {
        project,
        env_name,
        env,
        path,
        plan,
        tweaks,
    } = match prepare(target) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let violations = plan.limit_violations(&env);
    if !violations.is_empty() {
        eprintln!("✗ {} supera los límites de `{env_name}`:", plan.scenario);
        for v in violations {
            eprintln!("  {v}");
        }
        return exit::REFUSED;
    }
    let plan = Arc::new(plan);

    let hosts: Vec<&str> = plan.hosts.iter().map(String::as_str).collect();
    eprintln!("{} → {env_name} ({})", plan.scenario, hosts.join(", "));
    eprintln!("  {} · {}", plan.load.describe(), plan.rate_summary());
    if let Some(think) = plan.think_summary() {
        eprintln!("  pausas: {think}");
    }
    if !tweaks.is_empty() {
        eprintln!("  ajustes: {}", tweaks.join(" "));
    }
    for d in &plan.data {
        eprintln!("  datos: {}", d.dataset.describe());
    }
    let (duration, graceful_stop) = (plan.load.duration(), plan.load.graceful_stop());

    let Ok(approval) = crate::approve::verify_for_run(&project, &plan, &env, &env_name) else {
        return exit::REFUSED;
    };
    eprintln!(
        "  aprobado por {} el {}",
        approval.approved_by, approval.approved_at
    );

    timer::high_resolution();
    let started_at = jiff::Zoned::now();
    let progress = Arc::new(Progress::default());
    let runtime = tokio::runtime::Runtime::new().expect("runtime de tokio");
    let result = runtime.block_on(async {
        let ticker = std::io::stderr()
            .is_terminal()
            .then(|| tokio::spawn(show_progress(progress.clone(), duration + graceful_stop)));
        let result = engine::run(plan.clone(), progress).await;
        if let Some(t) = ticker {
            t.abort();
            eprintln!();
        }
        result
    });

    let run_id = started_at.strftime("%Y%m%d-%H%M%S").to_string();
    let mut doc = results_document(&run_id, &started_at, &path, &env_name, &plan, &result);
    if !tweaks.is_empty() {
        doc["tweaks"] = json!(tweaks);
    }
    doc["approval"] = json!({
        "hash": approval.hash,
        "approved_by": approval.approved_by,
        "approved_at": approval.approved_at,
    });
    let saved = save(&project, &run_id, &doc);

    if as_json {
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
    } else {
        print_summary(&result);
    }
    match saved {
        Ok(file) => {
            eprintln!("resultados: {}", display(&file));
            if result.passed {
                exit::OK
            } else {
                exit::FAILED
            }
        }
        Err(e) => {
            eprintln!("✗ no se pudieron guardar los resultados: {e}");
            exit::RUNTIME
        }
    }
}

async fn show_progress(p: Arc<Progress>, total: Duration) {
    let start = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        eprint!(
            "\r  {:>4}s / {}s  iteraciones {}  descartadas {}  requests {}  fallas {}   ",
            start.elapsed().as_secs(),
            total.as_secs(),
            p.started.load(Relaxed),
            p.dropped.load(Relaxed),
            p.requests.load(Relaxed),
            p.failed.load(Relaxed),
        );
    }
}

fn results_document(
    run_id: &str,
    started_at: &jiff::Zoned,
    path: &Path,
    env_name: &str,
    plan: &Plan,
    result: &RunResult,
) -> Value {
    let mut load = plan.load.canonical();
    load["max_request_rate"] = json!(plan.max_request_rate);
    // Con pausas distintas, `lt compare` no empareja los segundos. Solo si hay pausas,
    // para que las corridas de antes sigan siendo comparables.
    if let Some(think) = plan.think_summary() {
        load["think"] = json!(think);
    }
    let mut doc = json!({
        "format": JSON_FORMAT,
        "run_id": run_id,
        "scenario": plan.scenario,
        "scenario_file": display(path),
        "environment": env_name,
        "started_at": started_at.timestamp().to_string(),
        "load": load,
    });
    let Value::Object(result) = serde_json::to_value(result).unwrap() else {
        unreachable!()
    };
    doc.as_object_mut().unwrap().extend(result);
    doc
}

fn save(project: &Project, run_id: &str, doc: &Value) -> std::io::Result<std::path::PathBuf> {
    project.ensure_gitignore()?;
    let dir = project.runs_dir().join(run_id);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("results.json");
    std::fs::write(&file, serde_json::to_string_pretty(doc).unwrap())?;
    Ok(file)
}

fn print_summary(r: &RunResult) {
    let t = &r.totals;
    println!();
    for s in &r.setup {
        let status = s.status.map_or("-".into(), |c| c.to_string());
        let extracted = if s.extracted.is_empty() {
            String::new()
        } else {
            format!("  → {}", s.extracted.join(", "))
        };
        println!(
            "setup  {}  {status}  {} ms{extracted}",
            s.name, s.duration_ms
        );
    }
    if let Some(reason) = &r.aborted {
        println!("✗ {reason}. No se generó carga.");
        return;
    }
    println!(
        "iteraciones  planificadas {}  iniciadas {}  descartadas {}  interrumpidas {}  ({}/s)",
        t.planned, t.started, t.dropped, t.interrupted, t.iteration_rate
    );
    println!(
        "requests     {}  fallidos {}  no-2xx {}  ({}/s)  duración {}s",
        t.requests, t.failed, t.non_2xx, t.request_rate, r.duration_s
    );
    let checks: Vec<(&str, &CheckResult)> = r
        .steps
        .iter()
        .flat_map(|s| s.checks.iter().map(move |c| (s.name.as_str(), c)))
        .collect();
    let check_lines = check_summary(&checks);
    if let Some(total) = check_lines.first() {
        println!("{total}");
    }
    println!();

    let width = r
        .steps
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!(
        "{:<width$} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "paso", "req", "fallas", "no-2xx", "p50", "p90", "p95", "p99", "p99.9", "max"
    );
    for s in &r.steps {
        print!(
            "{:<width$} {:>7} {:>7} {:>7}",
            s.name, s.requests, s.failed, s.non_2xx
        );
        match &s.latency_ms {
            Some(l) => println!(
                " {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2}",
                l.p50, l.p90, l.p95, l.p99, l.p999, l.max
            ),
            None => println!(" {:>8}", "-"),
        }
    }
    println!(
        "{:>width$}  latencia en ms, desde el instante planificado",
        ""
    );

    for s in &r.steps {
        let only_200 = s.status_codes.len() == 1 && s.status_codes.contains_key("200");
        if !only_200 && !s.status_codes.is_empty() {
            let codes: Vec<_> = s
                .status_codes
                .iter()
                .map(|(c, n)| format!("{c}×{n}"))
                .collect();
            println!("códigos  {}: {}", s.name, codes.join("  "));
        }
        if !s.failures.is_empty() {
            let kinds: Vec<_> = s
                .failures
                .iter()
                .map(|(k, n)| format!("{}×{n}", serde_json::to_value(k).unwrap().as_str().unwrap()))
                .collect();
            println!("fallas   {}: {}", s.name, kinds.join("  "));
            for e in &s.error_samples {
                println!("         {e}");
            }
        }
    }
    if check_lines.len() > 1 {
        println!();
        for line in &check_lines[1..] {
            println!("{line}");
        }
    }
    if r.aborted.is_none() {
        println!();
        crate::findings::print_findings(&serde_json::to_value(&r.findings).unwrap());
    }
    if !r.thresholds.is_empty() {
        println!();
        for t in &r.thresholds {
            let mark = if t.passed { '✓' } else { '✗' };
            let actual = t
                .actual
                .map_or("sin datos".to_string(), |a| format!("{a} {}", t.unit));
            println!("{mark} {}  (medido: {actual})", t.threshold);
        }
        let failed = r.thresholds.iter().filter(|t| !t.passed).count();
        if failed > 0 {
            println!("{failed} threshold(s) no se cumplieron.");
        }
    }
    for w in &r.warnings {
        println!("AVISO: {w}");
    }
    println!();
}

/// Total de checks y, si alguno falló, cuál, en qué paso, cuántas veces y por qué.
/// La primera línea es el total; vacío si el escenario no tiene checks.
fn check_summary(checks: &[(&str, &CheckResult)]) -> Vec<String> {
    let passed: u64 = checks.iter().map(|(_, c)| c.passed).sum();
    let failed: u64 = checks.iter().map(|(_, c)| c.failed).sum();
    if passed + failed == 0 {
        return vec![];
    }
    let mut lines = vec![format!(
        "checks       {}  pasados {passed}  fallidos {failed}  ({})",
        passed + failed,
        percent(passed, passed + failed)
    )];
    let failing: Vec<_> = checks.iter().filter(|(_, c)| c.failed > 0).collect();
    if failing.is_empty() {
        return lines;
    }
    lines.push(format!("✗ {} check(s) fallaron:", failing.len()));
    let width = failing.iter().map(|(s, _)| s.len()).max().unwrap_or(0);
    for (step, c) in failing {
        let total = c.passed + c.failed;
        lines.push(format!(
            "  {step:<width$}  {}  ✗ {} de {total} ({})",
            c.check,
            c.failed,
            percent(c.failed, total)
        ));
        for why in &c.samples {
            lines.push(format!("  {:<width$}    {why}", ""));
        }
    }
    lines
}

/// `98.5%`, sin decimales de más.
fn percent(part: u64, total: u64) -> String {
    let p = part as f64 * 100.0 / total as f64;
    if p.fract() == 0.0 {
        format!("{p:.0}%")
    } else {
        format!("{p:.1}%")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, passed: u64, failed: u64, samples: &[&str]) -> CheckResult {
        CheckResult {
            check: name.into(),
            passed,
            failed,
            samples: samples.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn all_checks_pass_in_one_line() {
        let ok = check("status == 200", 50, 0, &[]);
        assert_eq!(
            check_summary(&[("login", &ok), ("get_product", &ok)]),
            ["checks       100  pasados 100  fallidos 0  (100%)"]
        );
        assert!(check_summary(&[]).is_empty());
    }

    #[test]
    fn lists_each_failing_check_with_its_step_and_reasons() {
        let ok = check("status == 200", 50, 0, &[]);
        let status = check("status == 201", 47, 3, &["status 500", "status 503"]);
        let id = check("$.id exists", 49, 1, &["$.id no existe"]);
        assert_eq!(
            check_summary(&[("login", &ok), ("create_order", &status), ("get", &id)]),
            [
                "checks       150  pasados 146  fallidos 4  (97.3%)",
                "✗ 2 check(s) fallaron:",
                "  create_order  status == 201  ✗ 3 de 50 (6%)",
                "                  status 500",
                "                  status 503",
                "  get           $.id exists  ✗ 1 de 50 (2%)",
                "                  $.id no existe",
            ]
        );
    }
}
