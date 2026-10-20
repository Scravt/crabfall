//! Lo que se dibuja en cada pantalla de la TUI. Solo lee el estado de `App`.

use std::sync::atomic::Ordering::Relaxed;

use crabfall_core::{
    engine::{Phase, Progress},
    i18n::pick,
    metrics::LiveSecond,
    tr,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Cell, Gauge, Paragraph, Row as TableRow, Sparkline, Table, Tabs, Wrap},
};

use super::{App, Ask, Detail, LastRun, ResultsView, RunView, Screen, Status};

/// Pestañas de los resultados.
pub const TABS: usize = 4;

const GOOD: Color = Color::Green;
const BAD: Color = Color::Red;
const WARN: Color = Color::Yellow;
const MUTED: Color = Color::DarkGray;

pub fn draw(frame: &mut Frame, app: &App) {
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    let env = app.env_name();
    let project = app
        .project
        .dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    frame.render_widget(
        Line::from(vec![
            " Crabfall ".bold().reversed(),
            Span::raw(format!("  {project}  ")),
            Span::styled(
                tr!("environment: {env}", "entorno: {env}"),
                Style::new().bold(),
            ),
        ]),
        top,
    );

    let keys = match &app.screen {
        Screen::Home => pick(
            "↑↓ choose · Enter open · e change environment · q quit",
            "↑↓ elegir · Enter abrir · e cambiar de entorno · q salir",
        ),
        Screen::Detail(d) if d.form.editing => pick(
            "type the value · Enter apply · Esc cancel",
            "escribí el valor · Enter aplicar · Esc cancelar",
        ),
        Screen::Detail(_) => pick(
            "↑↓ tweak · Enter edit · Del clear · c check · a approve · r run · o last report · Esc back",
            "↑↓ ajuste · Enter editar · Supr borrar · c check · a aprobar · r correr · o último reporte · Esc volver",
        ),
        Screen::Check(..) => pick("Esc back", "Esc volver"),
        Screen::Approve(..) => pick(
            "type the host and Enter to approve · Esc cancel",
            "escribí el host y Enter para aprobar · Esc cancelar",
        ),
        Screen::Running(..) => pick("q stop", "q frenar"),
        Screen::Results(..) => pick(
            "←→ tabs · o open the HTML report · r run again · Esc back",
            "←→ pestañas · o abrir el reporte HTML · r correr de nuevo · Esc volver",
        ),
    };
    let message = app
        .message
        .as_ref()
        .map_or(Line::default(), |(error, text)| {
            Line::styled(
                text.clone(),
                Style::new().fg(if *error { BAD } else { GOOD }),
            )
        });
    frame.render_widget(
        Paragraph::new(vec![message, Line::styled(keys, Style::new().fg(MUTED))]),
        bottom,
    );

    match &app.screen {
        Screen::Home => home(frame, body, app),
        Screen::Detail(d) => detail(frame, body, d),
        Screen::Check(d, c) => check(frame, body, d, c.result.as_ref()),
        Screen::Approve(d, a) => {
            let mut lines: Vec<Line> = a.summary.iter().map(|l| Line::raw(l.clone())).collect();
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled(
                    tr!(
                        "To approve, type the target host ({}): ",
                        "Para aprobar, escribí el host destino ({}): ",
                        a.host
                    ),
                    Style::new().bold(),
                ),
                Span::raw(format!("{}▌", a.input)),
            ]));
            frame.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                    Block::bordered().title(format!(
                        " {} · {} ",
                        pick("Approve", "Aprobar"),
                        d.name
                    )),
                ),
                body,
            );
        }
        Screen::Running(d, r) => running(frame, body, d, r),
        Screen::Results(d, r) => results(frame, body, d, r),
    }
}

fn home(frame: &mut Frame, area: Rect, app: &App) {
    if app.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(tr!(
                "There are no scenarios in .crabfall/scenarios/. Ask Claude for one, or write it by hand.",
                "No hay escenarios en .crabfall/scenarios/. Pedile uno a Claude, o escribilo a mano."
            ))
            .block(Block::bordered()),
            area,
        );
        return;
    }
    let rows = app.rows.iter().enumerate().map(|(i, row)| {
        let (status, color) = match &row.status {
            Status::Approved(flags) if flags.is_empty() => {
                (pick("✓ approved", "✓ aprobado").to_string(), GOOD)
            }
            Status::Approved(flags) => (
                tr!("✓ approved with {}", "✓ aprobado con {}", flags.join(" ")),
                GOOD,
            ),
            Status::Changed => (
                pick("⚠ changed since approved", "⚠ cambió desde que se aprobó").to_string(),
                WARN,
            ),
            Status::NotApproved => (pick("· not approved", "· sin aprobar").to_string(), MUTED),
            Status::Invalid(why) => (format!("✗ {why}"), BAD),
        };
        let mut style = Style::new();
        if i == app.selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        TableRow::new(vec![
            Cell::from(row.name.clone()).bold(),
            Cell::from(status).fg(color),
            last_run(row.last.as_ref()),
            Cell::from(row.description.clone().unwrap_or_default()).fg(MUTED),
        ])
        .style(style)
    });
    let header = TableRow::new(vec![
        pick("scenario", "escenario"),
        pick("approval", "aprobación"),
        pick("last run", "última corrida"),
        pick("description", "descripción"),
    ])
    .style(Style::new().fg(MUTED));
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Length(48),
                Constraint::Length(20),
                Constraint::Min(10),
            ],
        )
        .header(header)
        .block(Block::bordered().title(pick(" Scenarios ", " Escenarios "))),
        area,
    );
}

fn last_run(last: Option<&LastRun>) -> Cell<'static> {
    match last {
        None => Cell::from("—").fg(MUTED),
        Some(l) if l.stopped => Cell::from(format!("■ {}", l.id)).fg(WARN),
        Some(l) if l.passed => Cell::from(format!("✓ {}", l.id)).fg(GOOD),
        Some(l) => Cell::from(format!("✗ {}", l.id)).fg(BAD),
    }
}

fn detail(frame: &mut Frame, area: Rect, d: &Detail) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(area);
    let summary: Vec<Line> = d
        .summary
        .iter()
        .map(|l| {
            if l.starts_with('✗') {
                Line::styled(l.clone(), Style::new().fg(BAD))
            } else {
                Line::raw(l.clone())
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(summary)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(format!(" {} ", d.name))),
        left,
    );

    let [form_area, state_area] = Layout::vertical([
        Constraint::Length(d.form.fields.len() as u16 + 2),
        Constraint::Min(0),
    ])
    .areas(right);
    let fields: Vec<Line> = d
        .form
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let selected = i == d.form.selected;
            let editing = selected && d.form.editing;
            let error = d.field_error.as_ref().is_some_and(|(e, _)| *e == i);
            let value = if editing {
                format!("{}▌", f.value)
            } else if f.value.is_empty() {
                "·".into()
            } else {
                f.value.clone()
            };
            let mut line = Line::from(vec![
                Span::raw(if selected { "› " } else { "  " }),
                Span::styled(format!("{:<14}", f.label), Style::new().fg(MUTED)),
                Span::styled(
                    value,
                    if error {
                        Style::new().fg(BAD)
                    } else {
                        Style::new().bold()
                    },
                ),
            ]);
            if selected && !editing {
                line = line.style(Style::new().add_modifier(Modifier::REVERSED));
            }
            line
        })
        .collect();
    frame.render_widget(
        Paragraph::new(fields).block(Block::bordered().title(pick(
            " Tweaks (empty: as in the scenario) ",
            " Ajustes (vacío: como en el escenario) ",
        ))),
        form_area,
    );

    let mut state: Vec<Line> = Vec::new();
    match (&d.prepared, &d.approval) {
        (Ok(p), Ok(_)) => {
            state.push(Line::styled(
                pick(
                    "✓ approved: you can run it (r)",
                    "✓ aprobado: se puede correr (r)",
                ),
                Style::new().fg(GOOD),
            ));
            state.push(Line::default());
            state.push(Line::styled(
                pick("Same as:", "Equivale a:"),
                Style::new().fg(MUTED),
            ));
            state.push(Line::raw(crate::approve::command(
                "run",
                &p.plan.scenario,
                &p.env_name,
                &p.tweaks,
            )));
        }
        (Ok(p), Err(lines)) => {
            state.push(Line::styled(
                pick("· approve it to run it (a)", "· aprobalo para correrlo (a)"),
                Style::new().fg(WARN),
            ));
            // El motivo y, si se aprobó con otros ajustes, cuáles; los comandos van abajo.
            for l in lines.iter().filter(|l| !l.contains("crabfall ")).take(4) {
                state.push(Line::styled(
                    l.trim_start_matches("✗ ").to_string(),
                    Style::new().fg(MUTED),
                ));
            }
            state.push(Line::default());
            state.push(Line::styled(
                pick("Same as:", "Equivale a:"),
                Style::new().fg(MUTED),
            ));
            state.push(Line::raw(crate::approve::command(
                "approve",
                &p.plan.scenario,
                &p.env_name,
                &p.tweaks,
            )));
        }
        (Err(_), _) => state.push(Line::styled(
            pick(
                "✗ it cannot be built: see the errors",
                "✗ no se puede armar: mirá los errores",
            ),
            Style::new().fg(BAD),
        )),
    }
    frame.render_widget(
        Paragraph::new(state)
            .wrap(Wrap { trim: false })
            .block(Block::bordered()),
        state_area,
    );
}

fn check(
    frame: &mut Frame,
    area: Rect,
    d: &Detail,
    result: Option<&Result<crabfall_core::check::CheckResult, String>>,
) {
    let mut lines: Vec<Line> = Vec::new();
    match result {
        None => lines.push(Line::raw(pick(
            "running each flow once, only against loopback…",
            "corriendo cada flow una vez, solo contra loopback…",
        ))),
        Some(Err(e)) => lines.push(Line::styled(format!("✗ {e}"), Style::new().fg(BAD))),
        Some(Ok(r)) => {
            for s in &r.steps {
                let (mark, color) = if s.ok { ("✓", GOOD) } else { ("✗", BAD) };
                let outcome = match (&s.status, &s.failure) {
                    (Some(code), _) => code.to_string(),
                    (None, Some(f)) => f.message.clone(),
                    (None, None) => "-".into(),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{mark} "), Style::new().fg(color)),
                    Span::styled(format!("{}/{}  ", s.flow, s.name), Style::new().bold()),
                    Span::raw(format!("{} {}  ", s.method, s.url)),
                    Span::styled(outcome, Style::new().fg(color)),
                    Span::styled(format!("  {} ms", s.duration_ms), Style::new().fg(MUTED)),
                ]));
                for to in &s.redirects {
                    lines.push(Line::styled(format!("    → {to}"), Style::new().fg(MUTED)));
                }
                for (name, value) in &s.extracted {
                    lines.push(Line::styled(
                        format!("    {name} = {value}"),
                        Style::new().fg(MUTED),
                    ));
                }
                for c in s.checks.iter().filter(|c| !c.passed) {
                    lines.push(Line::styled(
                        format!(
                            "    ✗ {}: {}",
                            c.check,
                            c.reason.as_deref().unwrap_or_default()
                        ),
                        Style::new().fg(BAD),
                    ));
                }
                if let (Some(f), Some(_)) = (&s.failure, s.status) {
                    lines.push(Line::styled(
                        format!("    {}", f.message),
                        Style::new().fg(BAD),
                    ));
                }
            }
            if !r.skipped.is_empty() {
                lines.push(Line::styled(
                    tr!("skipped: {}", "omitidos: {}", r.skipped.join(", ")),
                    Style::new().fg(MUTED),
                ));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(format!(" check · {} ", d.name))),
        area,
    );
}

fn running(frame: &mut Frame, area: Rect, d: &Detail, r: &RunView) {
    let p: &Progress = &r.progress;
    let Ok(prepared) = &d.prepared else { return };
    let plan = &prepared.plan;
    let live = p.live();
    let total = plan.load.duration().as_secs().max(1);
    let elapsed = super::since(p.load_start());

    let [head, gauge, numbers, charts, steps, notes] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(8),
        Constraint::Min(4),
        Constraint::Length(4),
    ])
    .areas(area);

    let phase = match p.phase() {
        Phase::Setup => pick("setup", "setup"),
        Phase::VuSetup => pick("preparing VUs (vu_setup)", "preparando VUs (vu_setup)"),
        Phase::Load => pick("load", "carga"),
        Phase::Draining => pick(
            "waiting for the iterations in progress",
            "esperando las iteraciones en curso",
        ),
        Phase::Done => pick("saving", "guardando"),
    };
    frame.render_widget(
        Line::from(vec![
            Span::styled(
                format!(" {} → {}  ", plan.scenario, prepared.env_name),
                Style::new().bold(),
            ),
            Span::raw(format!("{} · ", plan.load.describe())),
            Span::styled(phase, Style::new().fg(WARN)),
            Span::styled(
                format!("  · {}", plan.http_summary()),
                Style::new().fg(MUTED),
            ),
        ]),
        head,
    );
    let ratio = if p.phase() == Phase::VuSetup {
        let total = p.vus_total.load(Relaxed).max(1);
        p.vus_ready.load(Relaxed) as f64 / total as f64
    } else {
        (elapsed as f64 / total as f64).min(1.0)
    };
    let label = if p.phase() == Phase::VuSetup {
        format!(
            "{}/{} VUs",
            p.vus_ready.load(Relaxed),
            p.vus_total.load(Relaxed)
        )
    } else {
        format!("{elapsed}s / {total}s")
    };
    frame.render_widget(
        Gauge::default()
            .ratio(ratio)
            .label(label)
            .gauge_style(Style::new().fg(Color::Cyan)),
        gauge,
    );

    // El último segundo cerrado.
    let last = live.last();
    let (started, dropped, requests, errors, sent, received) =
        last.map_or((0, 0, 0, 0, 0, 0), |s| {
            let p = &s.point;
            (
                p.started,
                p.dropped,
                p.requests,
                p.errors,
                p.bytes_sent,
                p.bytes_received,
            )
        });
    let err_pct = if requests > 0 {
        errors as f64 * 100.0 / requests as f64
    } else {
        0.0
    };
    let total_dropped = p.dropped.load(Relaxed);
    let concurrency = plan.load.concurrency();
    let rate = plan
        .max_request_rate
        .map_or(String::new(), |_| plan.rate_summary());
    let number_lines = vec![
        Line::from(vec![
            Span::styled(
                pick("iterations/s ", "iteraciones/s "),
                Style::new().fg(MUTED),
            ),
            Span::styled(format!("{started:<6}"), Style::new().bold()),
            Span::styled(pick("dropped ", "descartadas "), Style::new().fg(MUTED)),
            Span::styled(
                format!("{dropped:<6}"),
                Style::new()
                    .bold()
                    .fg(if dropped > 0 { BAD } else { Color::Reset }),
            ),
            Span::styled(pick("busy VUs ", "VUs ocupados "), Style::new().fg(MUTED)),
            Span::styled(
                format!("{}/{concurrency}", p.busy.load(Relaxed)),
                Style::new().bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled("requests/s   ", Style::new().fg(MUTED)),
            Span::styled(format!("{requests:<6}"), Style::new().bold()),
            Span::styled(pick("errors ", "errores "), Style::new().fg(MUTED)),
            Span::styled(
                format!("{err_pct:.1}%  "),
                Style::new()
                    .bold()
                    .fg(if errors > 0 { BAD } else { Color::Reset }),
            ),
            Span::styled(pick("traffic ", "tráfico "), Style::new().fg(MUTED)),
            Span::styled(
                format!(
                    "↑{}/s ↓{}/s",
                    crabfall_core::upload::size(sent as usize),
                    crabfall_core::upload::size(received as usize)
                ),
                Style::new().bold(),
            ),
        ]),
        Line::styled(rate, Style::new().fg(MUTED)),
    ];
    frame.render_widget(
        Paragraph::new(number_lines)
            .block(Block::new().title(pick(" last second ", " último segundo "))),
        numbers,
    );

    let [p95_area, err_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(charts);
    let width = p95_area.width.saturating_sub(2) as usize;
    let tail = &live[live.len().saturating_sub(width)..];
    let p95: Vec<u64> = tail
        .iter()
        .map(|s| s.point.p95.unwrap_or(0.0).round() as u64)
        .collect();
    let errs: Vec<u64> = tail.iter().map(|s| s.point.errors).collect();
    let last_p95 = last
        .and_then(|s| s.point.p95)
        .map_or("—".into(), |v| format!("{v:.0} ms"));
    frame.render_widget(
        Sparkline::default()
            .data(&p95)
            .style(Style::new().fg(Color::Cyan))
            .block(Block::bordered().title(tr!(
                " p95 per second · {} ",
                " p95 por segundo · {} ",
                last_p95
            ))),
        p95_area,
    );
    frame.render_widget(
        Sparkline::default()
            .data(&errs)
            .style(Style::new().fg(BAD))
            .block(Block::bordered().title(tr!(
                " errors per second · {} ",
                " errores por segundo · {} ",
                errors
            ))),
        err_area,
    );

    step_table(frame, steps, plan, &live);

    let mut warnings: Vec<Line> = Vec::new();
    match r.ask {
        Ask::Stop => warnings.push(Line::styled(
            pick(
                "Stop? It stops starting iterations, waits for the ones in progress and saves what ran. (y/n)",
                "¿Frenar? Deja de iniciar iteraciones, espera las que están en curso y guarda lo que corrió. (s/n)",
            ),
            Style::new().bold().fg(WARN),
        )),
        Ask::Cut => warnings.push(Line::styled(
            pick("Cut the iterations in progress now? (y/n)", "¿Cortar ya las iteraciones en curso? (s/n)"),
            Style::new().bold().fg(BAD),
        )),
        Ask::Nothing => {}
    }
    if p.stop_requested() {
        warnings.push(Line::styled(
            pick(
                "stopping: waiting for the iterations in progress (q to cut them)",
                "frenando: espero a las iteraciones en curso (q para cortarlas)",
            ),
            Style::new().fg(WARN),
        ));
    }
    if total_dropped > 0 {
        warnings.push(Line::styled(
            tr!(
                "⚠ {} iterations dropped for lack of free VUs: the server cannot keep up or max_vus is low",
                "⚠ {} iteraciones descartadas por falta de VUs libres: el servidor no da abasto o max_vus es bajo",
                total_dropped
            ),
            Style::new().fg(WARN),
        ));
    }
    let (ready, vus) = (p.vus_ready.load(Relaxed), p.vus_total.load(Relaxed));
    if vus > 0 && ready < vus && p.phase() != Phase::VuSetup {
        warnings.push(Line::styled(
            tr!(
                "⚠ {} of {} VUs did not complete vu_setup",
                "⚠ {} de {} VUs no completaron vu_setup",
                vus - ready,
                vus
            ),
            Style::new().fg(WARN),
        ));
    }
    let refresh_failed = p.refresh_failed.load(Relaxed);
    if refresh_failed > 0 {
        warnings.push(Line::styled(
            tr!(
                "⚠ {} VUs failed to renew their session",
                "⚠ {} VUs fallaron al renovar la sesión",
                refresh_failed
            ),
            Style::new().fg(WARN),
        ));
    }
    frame.render_widget(Paragraph::new(warnings).wrap(Wrap { trim: false }), notes);
}

/// Pasos durante la corrida: requests y errores acumulados, p50 del último segundo y el
/// peor p95 por segundo de los últimos 5.
fn step_table(
    frame: &mut Frame,
    area: Rect,
    plan: &crabfall_core::engine::plan::Plan,
    live: &[LiveSecond],
) {
    let recent = &live[live.len().saturating_sub(5)..];
    let rows = plan.steps().enumerate().map(|(i, s)| {
        let requests: u64 = live.iter().map(|l| l.steps[i].requests).sum();
        let errors: u64 = live.iter().map(|l| l.steps[i].errors).sum();
        let err = if requests > 0 {
            format!("{:.1}%", errors as f64 * 100.0 / requests as f64)
        } else {
            "—".into()
        };
        let p50 = live
            .last()
            .and_then(|l| l.steps[i].p50)
            .map_or("—".into(), |v| format!("{v:.1}"));
        let p95 = recent
            .iter()
            .filter_map(|l| l.steps[i].p95)
            .reduce(f64::max)
            .map_or("—".into(), |v| format!("{v:.1}"));
        TableRow::new(vec![
            Cell::from(s.name.clone()).bold(),
            Cell::from(requests.to_string()),
            Cell::from(err).fg(if errors > 0 { BAD } else { Color::Reset }),
            Cell::from(p50),
            Cell::from(p95),
        ])
    });
    let header = TableRow::new(vec![
        pick("step", "paso"),
        "requests",
        pick("errors", "errores"),
        pick("p50 (last s)", "p50 (último s)"),
        pick("p95 (last 5 s)", "p95 (últimos 5 s)"),
    ])
    .style(Style::new().fg(MUTED));
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(28),
                Constraint::Length(10),
                Constraint::Length(9),
                Constraint::Length(16),
                Constraint::Min(18),
            ],
        )
        .header(header)
        .block(Block::bordered().title(pick(" Steps (ms) ", " Pasos (ms) "))),
        area,
    );
}

fn results(frame: &mut Frame, area: Rect, d: &Detail, r: &ResultsView) {
    let result = &r.finished.result;
    let [tabs_area, content] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let titles = vec![
        pick("1 Summary", "1 Resumen"),
        pick("2 Findings", "2 Hallazgos"),
        pick("3 Steps", "3 Pasos"),
        pick("4 Checks", "4 Checks"),
    ];
    frame.render_widget(
        Tabs::new(titles)
            .select(r.tab)
            .highlight_style(Style::new().bold().reversed()),
        tabs_area,
    );
    let block = Block::bordered().title(format!(" {} ", d.name));
    let mut lines: Vec<Line> = Vec::new();
    match r.tab {
        0 => {
            let (verdict, color) = if let Some(reason) = &result.aborted {
                (tr!("✗ aborted: {reason}", "✗ abortada: {reason}"), BAD)
            } else if let Some(at) = result.stopped_at_s {
                (
                    tr!(
                        "■ stopped by you at second {at}",
                        "■ la frenaste en el segundo {at}"
                    ),
                    WARN,
                )
            } else if result.passed {
                (pick("✓ passed", "✓ pasó").to_string(), GOOD)
            } else {
                (
                    pick("✗ thresholds not met", "✗ no se cumplieron los thresholds").to_string(),
                    BAD,
                )
            };
            lines.push(Line::styled(verdict, Style::new().bold().fg(color)));
            lines.push(Line::default());
            let t = &result.totals;
            lines.push(Line::raw(tr!(
                "iterations {} started · {} dropped · {:.1}/s",
                "iteraciones {} iniciadas · {} descartadas · {:.1}/s",
                t.started,
                t.dropped,
                t.iteration_rate
            )));
            lines.push(Line::raw(tr!(
                "requests   {} · {} failed · {} non-2xx · {:.1}/s · {:.1} s",
                "requests   {} · {} fallidos · {} no-2xx · {:.1}/s · {:.1} s",
                t.requests,
                t.failed,
                t.non_2xx,
                t.request_rate,
                result.duration_s
            )));
            lines.push(Line::raw(tr!(
                "traffic    sent {} · received {}",
                "tráfico    enviados {} · recibidos {}",
                crabfall_core::upload::size(t.bytes_sent as usize),
                crabfall_core::upload::size(t.bytes_received as usize)
            )));
            let checks = t.checks_passed + t.checks_failed;
            if checks > 0 {
                lines.push(Line::raw(tr!(
                    "checks     {} passed of {}",
                    "checks     {} pasados de {}",
                    t.checks_passed,
                    checks
                )));
            }
            for th in &result.thresholds {
                let (mark, color) = if th.passed {
                    ("✓", GOOD)
                } else {
                    ("✗", BAD)
                };
                let actual = th
                    .actual
                    .map_or(pick("no data", "sin datos").to_string(), |a| {
                        format!("{a} {}", th.unit)
                    });
                lines.push(Line::styled(
                    format!("{mark} {}  ({actual})", th.threshold),
                    Style::new().fg(color),
                ));
            }
            if !r.comparison.is_empty() {
                lines.push(Line::default());
                lines.extend(r.comparison.iter().map(|l| Line::raw(l.clone())));
            }
            for w in &result.warnings {
                lines.push(Line::styled(format!("⚠ {w}"), Style::new().fg(WARN)));
            }
            lines.push(Line::default());
            match &r.finished.results {
                Ok(file) => lines.push(Line::styled(
                    tr!("results: {}", "resultados: {}", crate::display(file)),
                    Style::new().fg(MUTED),
                )),
                Err(e) => lines.push(Line::styled(format!("✗ {e}"), Style::new().fg(BAD))),
            }
            if let Some(Ok(report)) = &r.finished.report {
                lines.push(Line::styled(
                    tr!(
                        "report:  {}  (o to open it)",
                        "reporte: {}  (o para abrirlo)",
                        crate::display(report)
                    ),
                    Style::new().fg(MUTED),
                ));
            }
        }
        1 => {
            if result.findings.is_empty() {
                lines.push(Line::raw(tr!(
                    "No findings: no known pattern was detected.",
                    "Sin hallazgos: no se detectó ningún patrón conocido."
                )));
            }
            for f in &result.findings {
                let json = serde_json::to_value(f).unwrap_or_default();
                lines.push(Line::styled(
                    json["title"].as_str().unwrap_or_default().to_string(),
                    Style::new().bold().fg(BAD),
                ));
                let source = &json["source"];
                let location: Vec<&str> = ["file", "symbol"]
                    .iter()
                    .filter_map(|k| source[*k].as_str())
                    .collect();
                if !location.is_empty() {
                    lines.push(Line::styled(
                        format!("   {}", location.join(" · ")),
                        Style::new().fg(MUTED),
                    ));
                }
                for cause in json["causes"].as_array().into_iter().flatten() {
                    lines.push(Line::raw(format!(
                        "   - {}",
                        cause.as_str().unwrap_or_default()
                    )));
                }
                lines.push(Line::default());
            }
        }
        2 => {
            let rows = result.steps.iter().map(|s| {
                let l = s.latency_ms.as_ref();
                let ms = |f: fn(&crabfall_core::metrics::Percentiles) -> f64| {
                    l.map_or("—".into(), |l| format!("{:.1}", f(l)))
                };
                TableRow::new(vec![
                    Cell::from(s.name.clone()).bold(),
                    Cell::from(s.requests.to_string()),
                    Cell::from(s.failed.to_string()),
                    Cell::from(s.non_2xx.to_string()),
                    Cell::from(ms(|l| l.p50)),
                    Cell::from(ms(|l| l.p95)),
                    Cell::from(ms(|l| l.p99)),
                    Cell::from(ms(|l| l.max)),
                ])
            });
            let header = TableRow::new(vec![
                pick("step", "paso"),
                "req",
                pick("failed", "fallas"),
                pick("non-2xx", "no-2xx"),
                "p50",
                "p95",
                "p99",
                "max",
            ])
            .style(Style::new().fg(MUTED));
            frame.render_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(28),
                        Constraint::Length(8),
                        Constraint::Length(8),
                        Constraint::Length(8),
                        Constraint::Length(9),
                        Constraint::Length(9),
                        Constraint::Length(9),
                        Constraint::Min(9),
                    ],
                )
                .header(header)
                .block(block.title_bottom(pick(" latency in ms ", " latencia en ms "))),
                content,
            );
            return;
        }
        _ => {
            let failing: Vec<_> = result
                .steps
                .iter()
                .flat_map(|s| s.checks.iter().map(move |c| (s, c)))
                .collect();
            if failing.is_empty() {
                lines.push(Line::raw(pick(
                    "The scenario has no checks.",
                    "El escenario no tiene checks.",
                )));
            }
            for (s, c) in failing {
                let (mark, color) = if c.failed == 0 {
                    ("✓", GOOD)
                } else {
                    ("✗", BAD)
                };
                lines.push(Line::styled(
                    tr!(
                        "{mark} {}  {}  {} passed · {} failed",
                        "{mark} {}  {}  {} pasados · {} fallidos",
                        s.name,
                        c.check,
                        c.passed,
                        c.failed
                    ),
                    Style::new().fg(color),
                ));
                for why in &c.samples {
                    lines.push(Line::styled(format!("     {why}"), Style::new().fg(MUTED)));
                }
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        content,
    );
}
