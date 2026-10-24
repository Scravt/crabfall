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
    widgets::{
        Block, Cell, Clear, Gauge, Paragraph, Row as TableRow, Sparkline, Table, Tabs, Wrap,
    },
};

use super::{App, Ask, Detail, HistoryView, LastRun, ResultsView, RunView, Screen, Status};

/// Pestañas de los resultados.
pub const TABS: usize = 4;

const GOOD: Color = Color::Green;
const BAD: Color = Color::Red;
const WARN: Color = Color::Yellow;
/// Texto secundario. Gris claro: el gris oscuro casi no se lee en terminales oscuras.
const MUTED: Color = Color::Gray;

pub fn draw(frame: &mut Frame, app: &App) {
    let keys = key_lines(keys(app), frame.area().width);
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1 + keys.len() as u16),
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

    let message = app
        .message
        .as_ref()
        .map_or(Line::default(), |(error, text)| {
            Line::styled(
                text.clone(),
                Style::new().fg(if *error { BAD } else { GOOD }),
            )
        });
    let mut footer = vec![message];
    footer.extend(
        keys.into_iter()
            .map(|l| Line::styled(l, Style::new().fg(MUTED))),
    );
    frame.render_widget(Paragraph::new(footer), bottom);

    match &app.screen {
        Screen::Home => home(frame, body, app),
        Screen::Detail(d) => detail(frame, body, app, d),
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
        Screen::History(d, h) => history(frame, body, d, h),
        Screen::Compare(_, _, c) => compare(frame, body, &c.comparison),
        Screen::Results(d, r) => results(frame, body, d, r),
    }
    if app.help {
        help(frame, frame.area(), &app.screen);
    }
}

/// Las teclas de la pantalla actual, separadas por ` · `.
fn keys(app: &App) -> &'static str {
    match &app.screen {
        Screen::Home => pick(
            "? help · ↑↓ or j/k move · Enter open the scenario · e change environment · q quit",
            "? ayuda · ↑↓ o j/k moverse · Enter abrir el escenario · e cambiar de entorno · q salir",
        ),
        Screen::Detail(d) if d.form.editing => pick(
            "type the value · Enter apply · Esc cancel",
            "escribí el valor · Enter aplicar · Esc cancelar",
        ),
        Screen::Detail(_) => pick(
            "? help · ↑↓ or j/k tweak · Enter edit it · Del clear it · c check · a approve · r run · h history · o last report · Esc back",
            "? ayuda · ↑↓ o j/k ajuste · Enter editarlo · Supr borrarlo · c check · a aprobar · r correr · h historial · o último reporte · Esc volver",
        ),
        Screen::Check(..) => pick(
            "? help · Esc back · m menu",
            "? ayuda · Esc volver · m menú",
        ),
        Screen::History(_, h) if h.base.is_some() => pick(
            "? help · ↑↓ or j/k move · Enter on another run: compare them · Enter on the same one or Esc: release it",
            "? ayuda · ↑↓ o j/k moverse · Enter en otra corrida: compararlas · Enter en la misma o Esc: soltarla",
        ),
        Screen::History(..) => pick(
            "? help · ↑↓ or j/k move · Enter choose to compare · c compare with the previous one · b set as baseline · o report · Esc back · m menu",
            "? ayuda · ↑↓ o j/k moverse · Enter elegir para comparar · c comparar con la anterior · b fijar como base · o reporte · Esc volver · m menú",
        ),
        Screen::Compare(..) => pick(
            "? help · Esc back to the history · m menu",
            "? ayuda · Esc volver al historial · m menú",
        ),
        Screen::Approve(..) => pick(
            "type the host and Enter to approve · Esc cancel",
            "escribí el host y Enter para aprobar · Esc cancelar",
        ),
        Screen::Running(..) => pick("? help · q stop", "? ayuda · q frenar"),
        Screen::Results(..) => pick(
            "? help · ←→ or 1-4 tabs · r run again · Esc back to the scenario · m menu · h history · o HTML report",
            "? ayuda · ←→ o 1-4 pestañas · r correr de nuevo · Esc volver al escenario · m menú · h historial · o reporte HTML",
        ),
    }
}

/// Parte las teclas entre sus partes (` · `) para que entren en `width`, nunca en medio de una:
/// cortadas al final, la tecla que no se ve no existe (pasó con `b` en el historial).
pub fn key_lines(keys: &str, width: u16) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for part in keys.split(" · ") {
        match lines.last_mut() {
            Some(line) if Span::raw(format!("{line} · {part}")).width() <= usize::from(width) => {
                line.push_str(" · ");
                line.push_str(part);
            }
            _ => lines.push(part.to_string()),
        }
    }
    lines
}

/// La ayuda de la pantalla actual (`?`), encima de todo.
fn help(frame: &mut Frame, area: Rect, screen: &Screen) {
    let (title, text) = match screen {
        Screen::Home => (
            pick(" Help · scenarios ", " Ayuda · escenarios "),
            pick(
                "Each row is a scenario from .crabfall/scenarios/: the requests to make and how much load to put.\n\n\
                 How it works, for every scenario:\n\
                 \u{2003}1. check (c): each step once, without load, to see that it answers well. Only against your machine (loopback).\n\
                 \u{2003}2. approve (a): you read what load it will make and against which host, and type the host to sign it. A person approves, never an agent.\n\
                 \u{2003}3. run (r): the load. You watch it live and can stop it with q.\n\
                 Then you look at the results, the HTML report and the history, where you compare runs.\n\n\
                 Columns: approval says whether it is approved in this environment (and with which tweaks); last run, how its last run ended (✓ passed, ✗ failed, ■ you stopped it).\n\n\
                 Keys: ↑↓ or j/k move · Enter open the selected scenario · e switch environment (local, staging…) · q quit.",
                "Cada fila es un escenario de .crabfall/scenarios/: las requests a hacer y cuánta carga poner.\n\n\
                 Cómo se usa, para cada escenario:\n\
                 \u{2003}1. check (c): cada paso una vez, sin carga, para ver que responda bien. Solo contra tu máquina (loopback).\n\
                 \u{2003}2. aprobar (a): leés qué carga va a hacer y contra qué host, y escribís el host para firmarla. Aprueba una persona, nunca un agente.\n\
                 \u{2003}3. correr (r): la carga. La mirás en vivo y la podés frenar con q.\n\
                 Después mirás los resultados, el reporte HTML y el historial, donde comparás corridas.\n\n\
                 Columnas: aprobación dice si está aprobado en este entorno (y con qué ajustes); última corrida, cómo terminó la última (✓ pasó, ✗ falló, ■ la frenaste).\n\n\
                 Teclas: ↑↓ o j/k moverse · Enter abrir el escenario elegido · e cambiar de entorno (local, staging…) · q salir.",
            ),
        ),
        Screen::Detail(_) => (
            pick(" Help · scenario ", " Ayuda · escenario "),
            pick(
                "Left: what the scenario will do (flows, load, limits). Right: tweaks, and what to do next.\n\n\
                 The line at the top shows the three steps and which one comes next: check (c), approve (a), run (r).\n\n\
                 Tweaks change the load without touching the YAML, only for this run: move with ↑↓ or j/k, Enter to type a value, Enter again to apply it, Del to clear it. Empty means \"as in the scenario\". Below the list you see what the selected tweak means and an example. Changing a tweak changes the plan: it must be approved again.\n\n\
                 Other keys: h history of runs · o open the last report in the browser · Esc or m back to the list.",
                "Izquierda: qué va a hacer el escenario (flows, carga, límites). Derecha: ajustes, y qué hacer después.\n\n\
                 La línea de arriba muestra los tres pasos y cuál sigue: check (c), aprobar (a), correr (r).\n\n\
                 Los ajustes cambian la carga sin tocar el YAML, solo para esta corrida: moverse con ↑↓ o j/k, Enter para escribir un valor, Enter de nuevo para aplicarlo, Supr para borrarlo. Vacío es \"como en el escenario\". Debajo de la lista ves qué significa el ajuste elegido y un ejemplo. Cambiar un ajuste cambia el plan: hay que aprobar de nuevo.\n\n\
                 Otras teclas: h historial de corridas · o abrir el último reporte en el navegador · Esc o m volver a la lista.",
            ),
        ),
        Screen::Check(..) => (
            pick(" Help · check ", " Ayuda · check "),
            pick(
                "Check runs each step of each flow once, without load, and only against your machine. ✓ means it answered and its checks passed; ✗ shows why not. Below each step: redirects and the values it extracted.\n\nIt does not need an approval. Esc goes back.",
                "El check corre cada paso de cada flow una vez, sin carga, y solo contra tu máquina. ✓ es que respondió y pasaron sus checks; ✗ muestra por qué no. Debajo de cada paso: las redirecciones y los valores que extrajo.\n\nNo necesita aprobación. Esc vuelve.",
            ),
        ),
        Screen::Approve(..) => (
            pick(" Help · approve ", " Ayuda · aprobar "),
            pick(
                "Read the summary: how much load, for how long, against which host. If it is right, type the host exactly and Enter: that signs the approval for this scenario, environment and tweaks. If the scenario changes, it must be approved again. Esc cancels without approving.",
                "Leé el resumen: cuánta carga, por cuánto tiempo, contra qué host. Si está bien, escribí el host tal cual y Enter: eso firma la aprobación para este escenario, entorno y ajustes. Si el escenario cambia, hay que aprobar de nuevo. Esc cancela sin aprobar.",
            ),
        ),
        Screen::Running(..) => (
            pick(" Help · run ", " Ayuda · corrida "),
            pick(
                "The load is running. Top: the progress. Numbers: the last second (iterations, requests, errors, traffic). Charts: p95 and errors per second. Table: each step.\n\n\
                 Dropped iterations mean there were no free VUs: the server is slow or max_vus is low.\n\n\
                 q stops: it asks first, then stops starting iterations, waits for the ones in progress and saves what ran. q again cuts them.",
                "La carga está corriendo. Arriba: el avance. Números: el último segundo (iteraciones, requests, errores, tráfico). Gráficos: p95 y errores por segundo. Tabla: cada paso.\n\n\
                 Iteraciones descartadas quiere decir que no había VUs libres: el servidor está lento o max_vus es bajo.\n\n\
                 q frena: primero pregunta, después deja de iniciar iteraciones, espera las que están en curso y guarda lo que corrió. q de nuevo las corta.",
            ),
        ),
        Screen::Results(..) => (
            pick(" Help · results ", " Ayuda · resultados "),
            pick(
                "1 Summary: whether it passed, totals, thresholds and the comparison with the previous run. 2 Findings: problems Crabfall recognized (saturation, slow queries…) and their likely causes. 3 Steps: latency per step. 4 Checks: which response checks failed and examples.\n\n\
                 ←→ or 1-4 switch tabs · r run again · Esc back to the scenario, to change the tweaks · m back to the menu, to pick another scenario · h history · o open the HTML report (charts).",
                "1 Resumen: si pasó, totales, thresholds y la comparación con la corrida anterior. 2 Hallazgos: problemas que Crabfall reconoció (saturación, consultas lentas…) y sus causas probables. 3 Pasos: latencia por paso. 4 Checks: qué chequeos de respuesta fallaron y ejemplos.\n\n\
                 ←→ o 1-4 cambiar de pestaña · r correr de nuevo · Esc volver al escenario, para cambiar los ajustes · m volver al menú, para elegir otro escenario · h historial · o abrir el reporte HTML (gráficos).",
            ),
        ),
        Screen::History(..) => (
            pick(" Help · history ", " Ayuda · historial "),
            pick(
                "The runs of this scenario in this environment, newest first.\n\n\
                 To compare two runs: Enter on one (it is marked ◆ chosen), move to the other, Enter again. The one you chose first is the \"before\". c compares the selected run with the one just before it.\n\n\
                 b sets the selected run as the baseline: the accepted reference that crabfall compare --baseline uses. It is saved in .crabfall/baselines/; commit it to share it.\n\n\
                 o opens the run's HTML report · Esc releases the chosen run, or goes back.",
                "Las corridas de este escenario en este entorno, la más nueva primero.\n\n\
                 Para comparar dos corridas: Enter en una (queda marcada ◆ elegida), te movés a la otra y Enter de nuevo. La que elegiste primero es el \"antes\". c compara la corrida seleccionada con la inmediatamente anterior.\n\n\
                 b fija la corrida seleccionada como base: la referencia aceptada que usa crabfall compare --baseline. Se guarda en .crabfall/baselines/; versionala para compartirla.\n\n\
                 o abre el reporte HTML de la corrida · Esc suelta la corrida elegida, o vuelve.",
            ),
        ),
        Screen::Compare(..) => (
            pick(" Help · comparison ", " Ayuda · comparación "),
            pick(
                "Top: the verdict (better, worse, no significant changes, mixed) and why. With the same load, it pairs second by second and discards noise; with different loads, it only compares aggregates.\n\n\
                 Table: p95 before and now per step. ¿noise? means the difference is not clear enough to call it.\n\n\
                 Bottom: findings that are new, resolved or persist. Esc goes back to the history.",
                "Arriba: el veredicto (mejoró, empeoró, sin cambios significativos, mixto) y por qué. Con la misma carga, empareja segundo a segundo y descarta el ruido; con cargas distintas, solo compara agregados.\n\n\
                 Tabla: p95 antes y ahora por paso. ¿ruido? quiere decir que la diferencia no es lo bastante clara para afirmarla.\n\n\
                 Abajo: hallazgos nuevos, resueltos o que siguen. Esc vuelve al historial.",
            ),
        ),
    };
    let width = area.width.saturating_sub(8).min(100);
    let mut lines: Vec<Line> = text.lines().map(Line::raw).collect();
    lines.push(Line::default());
    lines.push(Line::styled(
        pick(
            "any key closes this help",
            "cualquier tecla cierra esta ayuda",
        ),
        Style::new().fg(MUTED),
    ));
    // Alto según el texto: cada línea ocupa lo que mide dividido por el ancho, redondeado
    // para arriba (con algo de margen por cómo corta las palabras).
    let inner = width.saturating_sub(2).max(1) as usize;
    let needed: usize = lines
        .iter()
        .map(|l| (l.width() * 11 / 10).div_ceil(inner).max(1))
        .sum();
    let height = (needed as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(title)
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        popup,
    );
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
        ])
        .style(style)
    });
    let header = TableRow::new(vec![
        pick("scenario", "escenario"),
        pick("approval", "aprobación"),
        pick("last run", "última corrida"),
    ])
    .style(Style::new().fg(MUTED));
    // La tabla ocupa lo que necesita; abajo, la descripción completa del elegido.
    let [table_area, about_area] = Layout::vertical([
        Constraint::Length(app.rows.len() as u16 + 3),
        Constraint::Min(5),
    ])
    .areas(area);
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Length(50),
                Constraint::Min(20),
            ],
        )
        .header(header)
        .block(Block::bordered().title(pick(" Scenarios ", " Escenarios "))),
        table_area,
    );
    let row = &app.rows[app.selected];
    let mut about = vec![Line::raw(row.description.clone().unwrap_or_else(|| {
        pick("(no description)", "(sin descripción)").to_string()
    }))];
    // El error entero: en la columna no entra.
    if let Status::Invalid(why) = &row.status {
        about.push(Line::default());
        about.push(Line::styled(format!("✗ {why}"), Style::new().fg(BAD)));
    }
    about.push(Line::default());
    about.push(Line::styled(
        pick(
            "Enter to open it: tweak the load, check it, approve it and run it. ? explains how it works.",
            "Enter para abrirlo: ajustar la carga, probarlo, aprobarlo y correrlo. ? explica cómo funciona.",
        ),
        Style::new().fg(MUTED),
    ));
    frame.render_widget(
        Paragraph::new(about)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(format!(" {} ", row.name))),
        about_area,
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

/// Cómo está un paso del camino check → aprobar → correr.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Pending,
    Done,
    Failed,
    /// No aplica (el check solo corre contra loopback).
    Skip,
}

/// Los tres pasos, cuál sigue y qué hacer.
fn next_step(app: &App, d: &Detail) -> ([Mark; 3], Option<usize>, String) {
    let Ok(p) = &d.prepared else {
        let hint = match &d.field_error {
            Some((i, _)) => tr!(
                "the tweak `{}` cannot be understood: Enter to fix it or Del to clear it",
                "el ajuste `{}` no se entiende: Enter para corregirlo o Supr para borrarlo",
                d.form.fields[*i].label
            ),
            None => tr!(
                "the scenario has errors (on the left): fix the YAML and open it again",
                "el escenario tiene errores (a la izquierda): corregí el YAML y abrilo de nuevo"
            ),
        };
        return ([Mark::Pending; 3], None, hint);
    };
    let check = if !p.plan.loopback_only() {
        Mark::Skip
    } else {
        match app.checks.get(&d.name) {
            Some(true) => Mark::Done,
            Some(false) => Mark::Failed,
            None => Mark::Pending,
        }
    };
    let approved = d.approval.is_ok();
    let marks = [
        check,
        if approved { Mark::Done } else { Mark::Pending },
        Mark::Pending,
    ];
    let changed = !approved
        && d.form.fields.iter().all(|f| f.value.is_empty())
        && app
            .rows
            .iter()
            .any(|r| r.name == d.name && matches!(r.status, Status::Changed));
    let (next, hint) = if approved {
        (
            2,
            tr!(
                "r to run the load: you watch it live and can stop it with q",
                "r para correr la carga: la mirás en vivo y la podés frenar con q"
            ),
        )
    } else if check == Mark::Pending {
        (
            0,
            tr!(
                "c to try each step once, without load (recommended before approving)",
                "c para probar cada paso una vez, sin carga (conviene antes de aprobar)"
            ),
        )
    } else if check == Mark::Failed {
        (
            0,
            tr!(
                "the check failed: look at the scenario or the server and c to try again (or a to approve anyway)",
                "el check falló: revisá el escenario o el servidor y c para probar de nuevo (o a para aprobar igual)"
            ),
        )
    } else if changed {
        (
            1,
            tr!(
                "the scenario changed since it was approved: a to read the new summary and approve it again",
                "el escenario cambió desde que se aprobó: a para leer el resumen nuevo y aprobarlo de nuevo"
            ),
        )
    } else {
        (
            1,
            tr!(
                "a to approve: you read what load it will make and type the host to sign it",
                "a para aprobar: leés qué carga va a hacer y escribís el host para firmarla"
            ),
        )
    };
    (marks, Some(next), hint)
}

fn steps_bar(frame: &mut Frame, area: Rect, app: &App, d: &Detail) {
    let (marks, next, hint) = next_step(app, d);
    let names = [
        pick("① check (c)", "① check (c)"),
        pick("② approve (a)", "② aprobar (a)"),
        pick("③ run (r)", "③ correr (r)"),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (i, (name, mark)) in names.iter().zip(marks).enumerate() {
        let (symbol, color) = match mark {
            Mark::Done => (" ✓", GOOD),
            Mark::Failed => (" ✗", BAD),
            Mark::Skip => (
                pick(" (n/a: not loopback)", " (no aplica: no es loopback)"),
                MUTED,
            ),
            Mark::Pending => ("", Color::Reset),
        };
        let mut style = Style::new().fg(if mark == Mark::Skip { MUTED } else { color });
        if next == Some(i) {
            style = style.bold().fg(Color::Cyan);
        }
        spans.push(Span::styled(format!("{name}{symbol}"), style));
        if i < 2 {
            spans.push(Span::styled("  →  ", Style::new().fg(MUTED)));
        }
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(spans),
            Line::from(vec![
                Span::styled(
                    pick(" Next: ", " Siguiente: "),
                    Style::new().bold().fg(Color::Cyan),
                ),
                Span::raw(hint),
            ]),
        ])
        .wrap(Wrap { trim: false }),
        area,
    );
}

/// Qué significa cada ajuste, con un ejemplo.
fn field_help(flag: &str) -> &'static str {
    match flag {
        "--executor" => pick(
            "Load model. constant_rate: a fixed number of iterations per second. ramping_rate: the rate follows the stages. constant_vus: N users, each repeating the flow. Ex: constant_vus",
            "Modelo de carga. constant_rate: una cantidad fija de iteraciones por segundo. ramping_rate: la tasa sigue las etapas. constant_vus: N usuarios, cada uno repitiendo el flow. Ej: constant_vus",
        ),
        "--rate" => pick(
            "Iterations per second, with constant_rate. Ex: 50/s or 600/m",
            "Iteraciones por segundo, con constant_rate. Ej: 50/s o 600/m",
        ),
        "--duration" => pick(
            "How long the load lasts. Ex: 30s or 2m",
            "Cuánto dura la carga. Ej: 30s o 2m",
        ),
        "--vus" => pick(
            "Users repeating the flow at the same time, with constant_vus. Ex: 20",
            "Usuarios repitiendo el flow a la vez, con constant_vus. Ej: 20",
        ),
        "--max-vus" => pick(
            "With a rate: how many iterations can be in flight at once. If all are busy, iterations are dropped. Ex: 100",
            "Con una tasa: cuántas iteraciones pueden estar en curso a la vez. Si están todas ocupadas, se descartan iteraciones. Ej: 100",
        ),
        "--start-rate" => pick(
            "Rate at the start of the ramp, with ramping_rate. Ex: 1/s",
            "Tasa al empezar la rampa, con ramping_rate. Ej: 1/s",
        ),
        "--stages" => pick(
            "Ramp stages, with ramping_rate: rate:duration separated by commas. Ex: 100/s:1m,0/s:30s",
            "Etapas de la rampa, con ramping_rate: tasa:duración separadas por comas. Ej: 100/s:1m,0/s:30s",
        ),
        "--graceful-stop" => pick(
            "At the end, how long to wait for the iterations still in flight. Ex: 10s",
            "Al terminar, cuánto esperar a las iteraciones que siguen en curso. Ej: 10s",
        ),
        "--think" => pick(
            "Pause after each step, fixed or a range; it replaces the scenario's. Ex: 1s or 1s-3s",
            "Pausa después de cada paso, fija o un rango; reemplaza la del escenario. Ej: 1s o 1s-3s",
        ),
        "--refresh-every" => pick(
            "How often each VU renews its session (it repeats vu_setup if there is no vu_refresh). Ex: 5m",
            "Cada cuánto renueva la sesión cada VU (repite vu_setup si no hay vu_refresh). Ej: 5m",
        ),
        "--threshold" => pick(
            "Pass conditions separated by spaces; they replace the scenario's. Ex: http.latency.p95<300ms http.error_rate<1%",
            "Condiciones para pasar, separadas por espacios; reemplazan las del escenario. Ej: http.latency.p95<300ms http.error_rate<1%",
        ),
        _ => "",
    }
}

fn detail(frame: &mut Frame, area: Rect, app: &App, d: &Detail) {
    let [bar, area] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    steps_bar(frame, bar, app, d);
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

    let [form_area, about_area, state_area] = Layout::vertical([
        Constraint::Length(d.form.fields.len() as u16 + 2),
        Constraint::Length(6),
        Constraint::Min(0),
    ])
    .areas(right);
    let field = &d.form.fields[d.form.selected];
    frame.render_widget(
        Paragraph::new(field_help(field.flag))
            .wrap(Wrap { trim: false })
            .fg(MUTED)
            .block(Block::bordered().title(format!(" {} ", field.label))),
        about_area,
    );
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
            lines.push(Line::default());
            lines.push(Line::styled(
                pick("What now?", "¿Y ahora?"),
                Style::new().bold().fg(Color::Cyan),
            ));
            for (key, what) in [
                (
                    "r",
                    pick("run it again, as it is", "correrlo de nuevo, igual"),
                ),
                (
                    "Esc",
                    pick(
                        "back to the scenario: change the tweaks and try other values",
                        "volver al escenario: cambiar los ajustes y probar otros valores",
                    ),
                ),
                (
                    "m",
                    pick(
                        "back to the menu: pick another scenario or environment",
                        "volver al menú: elegir otro escenario o entorno",
                    ),
                ),
                (
                    "h",
                    pick(
                        "history: compare with earlier runs",
                        "historial: comparar con corridas anteriores",
                    ),
                ),
                (
                    "o",
                    pick(
                        "open the HTML report, with charts",
                        "abrir el reporte HTML, con gráficos",
                    ),
                ),
                (
                    "2-4",
                    pick("findings, steps and checks", "hallazgos, pasos y checks"),
                ),
            ] {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {key:<4}"), Style::new().bold()),
                    Span::raw(what),
                ]));
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

fn history(frame: &mut Frame, area: Rect, d: &Detail, h: &HistoryView) {
    let rows = h.rows.iter().enumerate().map(|(i, row)| {
        let doc = &row.doc;
        let (result, color) = if doc["aborted"].is_string() {
            (pick("✗ aborted", "✗ abortada"), BAD)
        } else if doc["stopped_at_s"].is_number() {
            (pick("■ stopped", "■ frenada"), WARN)
        } else if doc["passed"].as_bool().unwrap_or(false) {
            (pick("✓ passed", "✓ pasó"), GOOD)
        } else {
            (pick("✗ failed", "✗ falló"), BAD)
        };
        // El peor p95 de los pasos: un número para ver la tendencia de un vistazo.
        let worst = doc["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["latency_ms"]["p95"].as_f64())
            .reduce(f64::max)
            .map_or("—".into(), |v| format!("{v:.1}"));
        let findings = doc["findings"].as_array().map_or(0, Vec::len);
        let is_baseline = h.baseline.as_deref() == Some(row.id.as_str());
        let mark = match (h.base == Some(i), is_baseline) {
            (true, true) => pick("◆ chosen · baseline", "◆ elegida · base fija"),
            (true, false) => pick("◆ chosen", "◆ elegida"),
            (false, true) => pick("baseline", "base fija"),
            (false, false) => "",
        };
        let mut style = Style::new();
        if i == h.selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        TableRow::new(vec![
            Cell::from(row.id.clone()).bold(),
            Cell::from(result).fg(color),
            Cell::from(format!("{} s", doc["duration_s"].as_f64().unwrap_or(0.0))),
            Cell::from(doc["totals"]["requests"].as_u64().unwrap_or(0).to_string()),
            Cell::from(format!(
                "{:.1}",
                doc["totals"]["request_rate"].as_f64().unwrap_or(0.0)
            )),
            Cell::from(worst),
            Cell::from(findings.to_string()).fg(if findings > 0 { WARN } else { Color::Reset }),
            Cell::from(mark).fg(Color::Cyan),
        ])
        .style(style)
    });
    let header = TableRow::new(vec![
        pick("run", "corrida"),
        pick("result", "resultado"),
        pick("duration", "duración"),
        "requests",
        "req/s",
        pick("worst p95 ms", "peor p95 ms"),
        pick("findings", "hallazgos"),
        "",
    ])
    .style(Style::new().fg(MUTED));
    frame.render_widget(
        Table::new(
            rows,
            [
                // Lo justo para cada columna (el id mide 15), así la tabla entra en 90 columnas sin
                // que ratatui achique todas y corte el id.
                Constraint::Length(15),
                Constraint::Length(11),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(7),
                Constraint::Length(11),
                Constraint::Length(9),
                Constraint::Min(9),
            ],
        )
        .header(header)
        .block(Block::bordered().title(tr!(" {} · history ", " {} · historial ", d.name))),
        area,
    );
}

fn compare(frame: &mut Frame, area: Rect, c: &crabfall_core::compare::Comparison) {
    use crabfall_core::compare::{Change, FindingState, Verdict};
    let [head, table, findings] = Layout::vertical([
        Constraint::Length(
            (3 + c.warnings.len() + c.regressions.len() + c.improvements.len()).min(14) as u16,
        ),
        Constraint::Min(5),
        Constraint::Length((c.findings.len() as u16 + 2).min(8)),
    ])
    .areas(area);

    let (verdict, color) = match c.verdict {
        Verdict::Better => (pick("better", "mejoró"), GOOD),
        Verdict::Worse => (pick("worse", "empeoró"), BAD),
        Verdict::Same => (
            pick("no significant changes", "sin cambios significativos"),
            Color::Reset,
        ),
        Verdict::Mixed => (
            pick(
                "mixed: improvements and regressions",
                "mixto: hay mejoras y regresiones",
            ),
            WARN,
        ),
    };
    let mut lines = vec![Line::from(vec![
        Span::raw(format!("{} → {}  ", c.base.run_id, c.new.run_id)),
        Span::styled(verdict, Style::new().bold().fg(color)),
        Span::styled(
            if c.same_load {
                tr!(
                    "  · same load: {} paired seconds",
                    "  · misma carga: {} segundos emparejados",
                    c.paired_windows
                )
            } else {
                tr!(
                    "  · different loads: aggregates only",
                    "  · cargas distintas: solo agregados"
                )
            },
            Style::new().fg(MUTED),
        ),
    ])];
    lines.extend(
        c.warnings
            .iter()
            .map(|w| Line::styled(format!("! {w}"), Style::new().fg(WARN))),
    );
    lines.extend(
        c.regressions
            .iter()
            .map(|r| Line::styled(format!("✗ {r}"), Style::new().fg(BAD))),
    );
    lines.extend(
        c.improvements
            .iter()
            .map(|i| Line::styled(format!("✓ {i}"), Style::new().fg(GOOD))),
    );
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered()),
        head,
    );

    let ms = |v: Option<f64>| v.map_or("—".to_string(), |v| format!("{v:.1}"));
    let rows = c.steps.iter().map(|s| {
        let (word, color) = match s.change {
            Change::Better => (pick("better", "mejoró"), GOOD),
            Change::Worse => (pick("worse", "empeoró"), BAD),
            Change::Same => (pick("same", "igual"), Color::Reset),
            Change::Uncertain => (pick("noise?", "¿ruido?"), WARN),
        };
        let change = s
            .p95_ms
            .change_pct
            .map_or("—".into(), |p| format!("{p:+.1}%"));
        TableRow::new(vec![
            Cell::from(s.name.clone()).bold(),
            Cell::from(ms(s.p95_ms.base)),
            Cell::from(ms(s.p95_ms.new)),
            Cell::from(change),
            Cell::from(word).fg(color),
            Cell::from(ms(s.error_pct.base) + " → " + &ms(s.error_pct.new)),
        ])
    });
    let header = TableRow::new(vec![
        pick("step", "paso"),
        pick("p95 before", "p95 antes"),
        pick("p95 now", "p95 ahora"),
        pick("change", "cambio"),
        pick("verdict", "veredicto"),
        pick("errors %", "errores %"),
    ])
    .style(Style::new().fg(MUTED));
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(28),
                Constraint::Length(11),
                Constraint::Length(11),
                Constraint::Length(10),
                Constraint::Length(11),
                Constraint::Min(14),
            ],
        )
        .header(header)
        .block(Block::bordered().title(pick(" Steps (ms) ", " Pasos (ms) "))),
        table,
    );

    let found: Vec<Line> = c
        .findings
        .iter()
        .map(|f| {
            let (state, color) = match f.state {
                FindingState::New => (pick("new     ", "nuevo   "), BAD),
                FindingState::Resolved => (pick("resolved", "resuelto"), GOOD),
                FindingState::Persists => (pick("persists", "sigue   "), WARN),
            };
            let title = f
                .new_title
                .as_ref()
                .or(f.base_title.as_ref())
                .cloned()
                .unwrap_or_default();
            Line::from(vec![
                Span::styled(format!("{state} "), Style::new().fg(color)),
                Span::raw(title),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(found)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(pick(" Findings ", " Hallazgos "))),
        findings,
    );
}
