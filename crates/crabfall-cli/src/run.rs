//! `crabfall run`: ejecuta un escenario y guarda los resultados en `.crabfall/runs/<id>/`.

use std::{
    io::IsTerminal,
    path::Path,
    sync::{Arc, atomic::Ordering::Relaxed},
    time::{Duration, Instant},
};

use crabfall_core::{
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
    if !plan.vu_setup.is_empty() {
        let names: Vec<&str> = plan.vu_setup.iter().map(|s| s.name.as_str()).collect();
        eprintln!(
            "  por VU: {} ({} VUs, antes de la carga)",
            names.join(", "),
            plan.load.concurrency()
        );
    }
    if let Some(refresh) = plan.refresh_summary() {
        eprintln!("  renovación: {refresh} por VU, en segundo plano");
    }
    if !tweaks.is_empty() {
        eprintln!("  ajustes: {}", tweaks.join(" "));
    }
    for u in plan.uploads_summary() {
        eprintln!("  archivo: {u}");
    }
    eprintln!("  http: {}", plan.http_summary());
    for d in &plan.data {
        eprintln!("  datos: {}", d.dataset.describe());
    }
    let (duration, graceful_stop) = (plan.load.duration(), plan.load.graceful_stop());

    let Ok(approval) = crate::approve::verify_for_run(&project, &plan, &env, &env_name, &tweaks)
    else {
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
            match crate::report::write(&project, &file) {
                Ok(report) => eprintln!("reporte:    {}", display(&report)),
                Err(e) => eprintln!("AVISO: no se generó el reporte: {e}"),
            }
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
    let mut start = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        // Mientras los VUs hacen su vu_setup, la carga todavía no empezó.
        let total_vus = p.vus_total.load(Relaxed);
        if total_vus > 0 && p.started.load(Relaxed) == 0 && p.dropped.load(Relaxed) == 0 {
            eprint!(
                "\r  preparando VUs (vu_setup) {}/{total_vus}   ",
                p.vus_ready.load(Relaxed)
            );
            start = Instant::now();
            continue;
        }
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
    // Con pausas distintas, `crabfall compare` no empareja los segundos. Solo si hay pausas,
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
    if let Some(v) = &r.vu_setup {
        let ok = if v.failed == 0 {
            "todos listos".to_string()
        } else {
            format!("{} quedaron afuera", v.failed)
        };
        println!(
            "vu_setup  {} VUs en {}: {ok}",
            v.vus,
            elapsed(v.duration_ms)
        );
        session_steps(&v.steps, "          ");
    }
    if let Some(v) = &r.vu_refresh {
        let lost = if v.failed_vus == 0 {
            "ningún VU perdido".to_string()
        } else {
            format!("{} VUs dejaron de iterar", v.failed_vus)
        };
        println!(
            "vu_refresh  cada {}: {} renovaciones, {lost}",
            crabfall_core::units::HumanDuration(std::time::Duration::from_secs_f64(v.every_s)),
            v.refreshes
        );
        session_steps(&v.steps, "            ");
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
    if t.bytes_sent + t.bytes_received > 0 {
        let points = &r.series.points;
        let peak =
            |f: fn(&crabfall_core::metrics::Point) -> u64| points.iter().map(f).max().unwrap_or(0);
        println!(
            "tráfico      enviados {}  recibidos {}",
            traffic(t.bytes_sent, t.send_rate, peak(|p| p.bytes_sent)),
            traffic(t.bytes_received, t.receive_rate, peak(|p| p.bytes_received))
        );
    }
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
        if let Some(l) = &s.rate_limit {
            let said: Vec<String> = [l.policy(), l.retry_after()]
                .into_iter()
                .flatten()
                .collect();
            if !said.is_empty() {
                println!("límite   {}: {}", s.name, said.join(" · "));
            }
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

/// Pasos de `vu_setup` o `vu_refresh`: requests, fallas, p50 y motivos.
fn session_steps(steps: &[crabfall_core::metrics::VuSetupStep], indent: &str) {
    for s in steps {
        let p50 = s
            .latency_ms
            .as_ref()
            .map_or("-".into(), |l| format!("{:.2} ms", l.p50));
        println!(
            "{indent}{}  {} requests  {} fallidos  p50 {p50}",
            s.name, s.requests, s.failed
        );
        for e in &s.errors {
            println!("{indent}  {e}");
        }
    }
}

/// `35 ms` o `2.4 s`.
/// `137.0 MB (2.1 MB/s, pico 4.3 MB/s = 36 Mbit/s)`. El pico va también en bits porque así
/// se miden los enlaces.
fn traffic(total: u64, rate: f64, peak: u64) -> String {
    use crabfall_core::upload::size;
    format!(
        "{} ({}/s, pico {}/s = {})",
        size(total as usize),
        size(rate as usize),
        size(peak as usize),
        bits(peak)
    )
}

/// Bytes por segundo en bits, como se miden los enlaces: `84 Mbit/s`, `1.0 Mbit/s`, `40 kbit/s`.
fn bits(bytes_per_s: u64) -> String {
    let bits = bytes_per_s as f64 * 8.0;
    if bits >= 10_000_000.0 {
        format!("{:.0} Mbit/s", bits / 1_000_000.0)
    } else if bits >= 1_000_000.0 {
        format!("{:.1} Mbit/s", bits / 1_000_000.0)
    } else {
        format!("{:.0} kbit/s", bits / 1_000.0)
    }
}

fn elapsed(ms: f64) -> String {
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.1} s", ms / 1000.0)
    }
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

    #[test]
    fn link_rates_use_the_unit_that_reads_best() {
        assert_eq!(bits(10_485_760), "84 Mbit/s");
        assert_eq!(bits(125_000), "1.0 Mbit/s");
        assert_eq!(bits(5 * 1024), "41 kbit/s");
    }

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
