//! `crabfall check`: smoke check sin aprobación, solo contra loopback (ADR-004 §4).

use serde_json::json;

use crate::{
    JSON_FORMAT, display, exit,
    prepare::{Prepared, prepare},
};

pub fn cmd_check(target: &crate::Target, as_json: bool) -> u8 {
    let Prepared {
        env_name,
        path,
        plan,
        ..
    } = match prepare(target) {
        Ok(p) => p,
        Err(code) => return code,
    };

    let runtime = tokio::runtime::Runtime::new().expect("runtime de tokio");
    let result = match runtime.block_on(crabfall_core::check::check(&plan)) {
        Ok(r) => r,
        Err(e) => {
            if as_json {
                let out = json!({ "format": JSON_FORMAT, "ok": false, "refused": e });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
            }
            eprintln!("✗ {e}");
            return exit::REFUSED;
        }
    };

    if as_json {
        let out = json!({
            "format": JSON_FORMAT,
            "scenario": plan.scenario,
            "scenario_file": display(&path),
            "environment": env_name,
            "ok": result.ok,
            "steps": result.steps,
            "skipped": result.skipped,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        println!("{} → {env_name}", plan.scenario);
        let width = result
            .steps
            .iter()
            .map(|s| s.flow.len() + s.name.len() + 1)
            .max()
            .unwrap_or(0);
        for s in &result.steps {
            let mark = if s.ok { '✓' } else { '✗' };
            let label = format!("{}/{}", s.flow, s.name);
            let outcome = match (&s.status, &s.failure) {
                (Some(code), _) => code.to_string(),
                (None, Some(f)) => format!("{:?}: {}", f.kind, f.message).to_lowercase(),
                (None, None) => "-".into(),
            };
            println!(
                "{mark} {label:<width$}  {} {}  {outcome}  {} ms",
                s.method, s.url, s.duration_ms
            );
            for to in &s.redirects {
                println!("    → {to}");
            }
            for (name, value) in &s.extracted {
                println!("    {name} = {value}");
            }
            for c in s.checks.iter().filter(|c| !c.passed) {
                println!(
                    "    ✗ {}: {}",
                    c.check,
                    c.reason.as_deref().unwrap_or_default()
                );
            }
            if let Some(f) = &s.failure
                && s.status.is_some()
            {
                println!("    {}", f.message);
            }
            if !s.ok
                && let Some(body) = &s.body
            {
                let preview: String = body.chars().take(300).collect();
                println!("    {}", preview.replace('\n', "\n    "));
            }
        }
        if !result.skipped.is_empty() {
            println!(
                "{}",
                crabfall_core::tr!("skipped: {}", "omitidos: {}", result.skipped.join(", "))
            );
        }
    }

    if result.ok { exit::OK } else { exit::FAILED }
}
