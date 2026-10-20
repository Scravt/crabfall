//! La TUI (ADR-015): `crabfall` sin argumentos, en una terminal. Elegir escenario y entorno,
//! ajustar, probar, aprobar, correr mirando en vivo, frenar y ver los resultados. Usa las
//! mismas piezas que los comandos (`prepare`, `approve`, `run::execute`): no son copias.

mod view;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use crabfall_core::{
    approval::Approval, check::CheckResult, engine::Progress, i18n::pick, overrides::Overrides,
    project::Project, tr, validate,
};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
};

use crate::{
    approve, exit,
    prepare::{Prepared, prepare_in},
    run::Finished,
};

/// Abre la TUI. Sale con 0, o con 2 si no hay proyecto.
pub fn run() -> u8 {
    let mut app = match App::load() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("✗ {e}");
            return exit::INVALID;
        }
    };
    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("✗ {e}");
        return exit::RUNTIME;
    }
    exit::OK
}

/// Un escenario de la lista.
struct Row {
    name: String,
    description: Option<String>,
    status: Status,
    last: Option<LastRun>,
}

/// Cómo está su aprobación en el entorno elegido.
enum Status {
    /// Aprobado; con los ajustes con que se aprobó, si los hubo.
    Approved(Vec<String>),
    /// Hay una aprobación, pero el escenario cambió desde entonces.
    Changed,
    NotApproved,
    /// No se puede armar (inválido, falta un secreto, un host fuera de allowed_hosts…).
    Invalid(String),
}

struct LastRun {
    id: String,
    passed: bool,
    stopped: bool,
}

/// Un ajuste del formulario: el flag y lo que escribió la persona.
struct Field {
    flag: &'static str,
    label: &'static str,
    value: String,
}

struct Form {
    fields: Vec<Field>,
    selected: usize,
    editing: bool,
    /// El valor antes de empezar a editar, para Esc.
    before: String,
}

impl Form {
    fn new() -> Self {
        let field = |flag, label| Field {
            flag,
            label,
            value: String::new(),
        };
        Self {
            fields: vec![
                field("--executor", "executor"),
                field("--rate", "rate"),
                field("--duration", "duration"),
                field("--vus", "vus"),
                field("--max-vus", "max_vus"),
                field("--start-rate", "start_rate"),
                field("--stages", "stages"),
                field("--graceful-stop", "graceful_stop"),
                field("--think", "think"),
                field("--refresh-every", "refresh_every"),
                field("--threshold", "thresholds"),
            ],
            selected: 0,
            editing: false,
            before: String::new(),
        }
    }

    /// El formulario con los ajustes de una aprobación (`--rate 100/s`, `--threshold "…"`).
    fn from_flags(flags: &[String]) -> Self {
        let mut form = Self::new();
        for flag in flags {
            let Some((name, value)) = flag.split_once(' ') else {
                continue;
            };
            let value = value.trim_matches('"').replace("\\\"", "\"");
            if let Some(field) = form.fields.iter_mut().find(|f| f.flag == name) {
                if !field.value.is_empty() {
                    field.value.push(' ');
                }
                field.value.push_str(&value);
            }
        }
        form
    }

    /// Los ajustes escritos, o el primer campo que no se entiende.
    fn overrides(&self) -> Result<Overrides, (usize, String)> {
        let mut o = Overrides::default();
        for (i, f) in self.fields.iter().enumerate() {
            let v = f.value.trim();
            if v.is_empty() {
                continue;
            }
            let err = |e: String| (i, e);
            match f.flag {
                "--executor" => o.executor = Some(v.parse().map_err(err)?),
                "--rate" => o.rate = Some(v.parse().map_err(err)?),
                "--duration" => o.duration = Some(v.parse().map_err(err)?),
                "--vus" => o.vus = Some(v.parse().map_err(|e| err(format!("{e}")))?),
                "--max-vus" => o.max_vus = Some(v.parse().map_err(|e| err(format!("{e}")))?),
                "--start-rate" => o.start_rate = Some(v.parse().map_err(err)?),
                "--stages" => {
                    o.stages = Some(crabfall_core::overrides::parse_stages(v).map_err(err)?)
                }
                "--graceful-stop" => o.graceful_stop = Some(v.parse().map_err(err)?),
                "--think" => o.think = Some(v.parse().map_err(err)?),
                "--refresh-every" => o.refresh_every = Some(v.parse().map_err(err)?),
                "--threshold" => {
                    o.thresholds = v
                        .split_whitespace()
                        .map(str::parse)
                        .collect::<Result<_, String>>()
                        .map_err(err)?
                }
                _ => {}
            }
        }
        Ok(o)
    }
}

/// Un escenario abierto: su plan con los ajustes, el resumen de riesgo y la aprobación.
struct Detail {
    name: String,
    form: Form,
    /// El plan armado, o por qué no se puede (en líneas).
    prepared: Result<Prepared, Vec<String>>,
    /// El campo del formulario que no se entiende.
    field_error: Option<(usize, String)>,
    summary: Vec<String>,
    approval: Result<Approval, Vec<String>>,
}

struct CheckView {
    rx: mpsc::Receiver<Result<CheckResult, String>>,
    result: Option<Result<CheckResult, String>>,
}

struct ApproveView {
    summary: Vec<String>,
    host: String,
    input: String,
}

/// Qué está preguntando la pantalla de la corrida.
#[derive(Clone, Copy, PartialEq)]
enum Ask {
    Nothing,
    /// ¿Frenar en orden?
    Stop,
    /// Ya está frenando: ¿cortar lo que está en curso?
    Cut,
}

struct RunView {
    progress: Arc<Progress>,
    rx: mpsc::Receiver<Finished>,
    ask: Ask,
}

struct ResultsView {
    finished: Finished,
    tab: usize,
    /// La comparación con la corrida anterior, en líneas.
    comparison: Vec<String>,
}

/// Una corrida guardada del escenario, para el historial.
struct HistoryRow {
    id: String,
    doc: serde_json::Value,
}

struct HistoryView {
    /// De la más reciente a la más vieja.
    rows: Vec<HistoryRow>,
    /// La corrida que es la base fija del escenario (`crabfall baseline`), si hay.
    baseline: Option<String>,
    selected: usize,
    /// La primera corrida elegida para comparar (Enter), si hay.
    base: Option<usize>,
}

struct CompareView {
    comparison: crabfall_core::compare::Comparison,
}

enum Screen {
    Home,
    History(Box<Detail>, HistoryView),
    Compare(Box<Detail>, HistoryView, Box<CompareView>),
    Detail(Box<Detail>),
    Check(Box<Detail>, CheckView),
    Approve(Box<Detail>, ApproveView),
    Running(Box<Detail>, RunView),
    Results(Box<Detail>, Box<ResultsView>),
}

struct App {
    project: Project,
    envs: Vec<String>,
    env: usize,
    rows: Vec<Row>,
    selected: usize,
    screen: Screen,
    /// Un aviso al pie: (es un error, texto).
    message: Option<(bool, String)>,
    /// Cómo salió el último check de cada escenario en esta sesión (pasó o no).
    checks: BTreeMap<String, bool>,
    /// La ayuda de la pantalla (`?`) está abierta.
    help: bool,
    quit: bool,
}

impl App {
    fn load() -> Result<Self, String> {
        let project = std::env::current_dir()
            .ok()
            .and_then(|cwd| Project::discover(&cwd))
            .ok_or_else(|| {
                tr!(
                    "no .crabfall/ found in this directory or its parents",
                    "no encontré .crabfall/ en este directorio ni en los superiores"
                )
            })?;
        Self::with_project(project)
    }

    fn with_project(project: Project) -> Result<Self, String> {
        let config = project.load_config()?;
        let envs: Vec<String> = config.environments.keys().cloned().collect();
        let env = config
            .default_environment
            .as_ref()
            .and_then(|d| envs.iter().position(|e| e == d))
            .unwrap_or(0);
        let mut app = Self {
            project,
            envs,
            env,
            rows: Vec::new(),
            selected: 0,
            screen: Screen::Home,
            message: None,
            checks: BTreeMap::new(),
            help: false,
            quit: false,
        };
        app.reload();
        Ok(app)
    }

    fn env_name(&self) -> &str {
        &self.envs[self.env]
    }

    /// Lee de nuevo los escenarios, sus aprobaciones y sus últimas corridas.
    fn reload(&mut self) {
        let last = last_runs(&self.project);
        let env = self.env_name().to_string();
        let mut names: Vec<String> = std::fs::read_dir(self.project.dir.join("scenarios"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "yaml" || x == "yml"))
            .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        names.sort();
        self.rows = names
            .into_iter()
            .map(|name| {
                let description =
                    std::fs::read_to_string(Project::scenario_path(Some(&self.project), &name))
                        .ok()
                        .and_then(|t| validate::parse_scenario(&t).ok())
                        .and_then(|s| s.description)
                        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "));
                let status = status(&self.project, &name, &env);
                let last = last
                    .get(&(name.clone(), env.clone()))
                    .map(|(id, doc)| LastRun {
                        id: id.clone(),
                        passed: doc["passed"].as_bool().unwrap_or(false),
                        stopped: doc["stopped_at_s"].is_number(),
                    });
                Row {
                    name,
                    description,
                    status,
                    last,
                }
            })
            .collect();
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        while !self.quit {
            self.poll_background();
            terminal.draw(|frame| view::draw(frame, self))?;
            if event::poll(Duration::from_millis(200))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key);
            }
        }
        // Al salir con una corrida en curso, se frena y se espera a que guarde.
        if let Screen::Running(_, run) = &self.screen {
            run.progress.stop();
            run.progress.stop();
            let _ = run.rx.recv();
        }
        Ok(())
    }

    /// Lo que terminó en segundo plano: un check o una corrida.
    fn poll_background(&mut self) {
        match &mut self.screen {
            Screen::Check(d, check) if check.result.is_none() => {
                if let Ok(r) = check.rx.try_recv() {
                    let passed = r
                        .as_ref()
                        .is_ok_and(|r| !r.steps.is_empty() && r.steps.iter().all(|s| s.ok));
                    self.checks.insert(d.name.clone(), passed);
                    check.result = Some(r);
                }
            }
            Screen::Running(_, run) => {
                let finished = match run.rx.try_recv() {
                    Ok(finished) => Some(finished),
                    Err(mpsc::TryRecvError::Empty) => None,
                    // El hilo terminó sin resultados (no pudo armar el plan): de vuelta al detalle.
                    Err(mpsc::TryRecvError::Disconnected) => {
                        let Screen::Running(detail, _) =
                            std::mem::replace(&mut self.screen, Screen::Home)
                        else {
                            unreachable!()
                        };
                        self.screen = Screen::Detail(detail);
                        self.error(tr!(
                            "the run could not start: the scenario changed while it was open",
                            "la corrida no pudo arrancar: el escenario cambió mientras estaba abierto"
                        ));
                        return;
                    }
                };
                if let Some(finished) = finished {
                    let Screen::Running(detail, _) =
                        std::mem::replace(&mut self.screen, Screen::Home)
                    else {
                        unreachable!()
                    };
                    let comparison = comparison(&self.project, &finished);
                    self.screen = Screen::Results(
                        detail,
                        Box::new(ResultsView {
                            finished,
                            tab: 0,
                            comparison,
                        }),
                    );
                    self.reload();
                }
            }
            _ => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            match &mut self.screen {
                // Ctrl+C en la corrida pide frenar, como en `crabfall run`.
                Screen::Running(_, run) => run.ask = Ask::Stop,
                _ => self.quit = true,
            }
            return;
        }
        // Con la ayuda abierta, cualquier tecla la cierra.
        if self.help {
            self.help = false;
            return;
        }
        if key.code == KeyCode::Char('?') && !self.typing() {
            self.help = true;
            return;
        }
        self.message = None;
        let screen = std::mem::replace(&mut self.screen, Screen::Home);
        self.screen = match screen {
            Screen::Home => self.on_home(key),
            Screen::Detail(d) => self.on_detail(d, key),
            Screen::Check(d, c) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') if c.result.is_some() => Screen::Detail(d),
                _ => Screen::Check(d, c),
            },
            Screen::Approve(d, a) => self.on_approve(d, a, key),
            Screen::History(d, h) => self.on_history(d, h, key),
            Screen::Compare(d, h, c) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Screen::History(d, h),
                _ => Screen::Compare(d, h, c),
            },
            Screen::Running(d, r) => self.on_running(d, r, key),
            Screen::Results(d, r) => self.on_results(d, r, key),
        };
    }

    /// La pantalla espera texto: `?` es parte de lo que se escribe, no la ayuda.
    fn typing(&self) -> bool {
        match &self.screen {
            Screen::Detail(d) => d.form.editing,
            Screen::Approve(..) => true,
            _ => false,
        }
    }

    fn on_home(&mut self, key: KeyEvent) -> Screen {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1))
            }
            KeyCode::Char('e') | KeyCode::Tab => {
                self.env = (self.env + 1) % self.envs.len();
                self.reload();
            }
            KeyCode::Enter => {
                if let Some(row) = self.rows.get(self.selected) {
                    // Si se aprobó con ajustes, se abre con esos mismos: así se puede correr.
                    let form = match &row.status {
                        Status::Approved(flags) => Form::from_flags(flags),
                        _ => Form::new(),
                    };
                    return Screen::Detail(Box::new(self.open(row.name.clone(), form)));
                }
            }
            _ => {}
        }
        Screen::Home
    }

    /// Arma el escenario con los ajustes del formulario.
    fn open(&self, name: String, form: Form) -> Detail {
        let env = self.env_name();
        let (prepared, field_error) = match form.overrides() {
            Ok(o) => (
                prepare_in(self.project.clone(), &name, Some(env), &[], &o),
                None,
            ),
            Err(e) => (Err(vec![format!("✗ {}", e.1)]), Some(e)),
        };
        let (summary, approval) = match &prepared {
            Ok(p) => {
                let (change, warning) = approve::change(&p.project, &p.plan, &p.env, &p.env_name);
                let mut summary: Vec<String> = warning.into_iter().collect();
                summary.extend(approve::summary(
                    &p.plan,
                    &p.env,
                    &p.env_name,
                    &change,
                    &p.tweaks,
                ));
                let approval = approve::verify(&p.project, &p.plan, &p.env, &p.env_name, &p.tweaks);
                (summary, approval)
            }
            Err(lines) => (lines.clone(), Err(vec![])),
        };
        Detail {
            name,
            form,
            prepared,
            field_error,
            summary,
            approval,
        }
    }

    fn on_detail(&mut self, d: Box<Detail>, key: KeyEvent) -> Screen {
        let mut d = d;
        if d.form.editing {
            let field = &mut d.form.fields[d.form.selected];
            match key.code {
                KeyCode::Enter => {
                    d.form.editing = false;
                    let Detail { name, form, .. } = *d;
                    return Screen::Detail(Box::new(self.open(name, form)));
                }
                KeyCode::Esc => {
                    field.value = std::mem::take(&mut d.form.before);
                    d.form.editing = false;
                }
                KeyCode::Backspace => {
                    field.value.pop();
                }
                KeyCode::Char(c) => field.value.push(c),
                _ => {}
            }
            return Screen::Detail(d);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => d.form.selected = d.form.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                d.form.selected = (d.form.selected + 1).min(d.form.fields.len() - 1)
            }
            KeyCode::Enter => {
                d.form.before = d.form.fields[d.form.selected].value.clone();
                d.form.editing = true;
            }
            KeyCode::Delete => {
                d.form.fields[d.form.selected].value.clear();
                let Detail { name, form, .. } = *d;
                return Screen::Detail(Box::new(self.open(name, form)));
            }
            KeyCode::Char('h') => return self.open_history(d),
            KeyCode::Char('c') => return self.start_check(d),
            KeyCode::Char('a') => return self.start_approve(d),
            KeyCode::Char('r') => return self.start_run(d),
            KeyCode::Char('o') => {
                let last = self
                    .rows
                    .iter()
                    .find(|r| r.name == d.name)
                    .and_then(|r| r.last.as_ref().map(|l| l.id.clone()));
                match last {
                    Some(id) => {
                        self.open_report(&self.project.runs_dir().join(id).join("report.html"))
                    }
                    None => self.error(tr!(
                        "this scenario has no runs in this environment yet",
                        "este escenario todavía no tiene corridas en este entorno"
                    )),
                }
            }
            _ => {}
        }
        Screen::Detail(d)
    }

    fn start_check(&mut self, d: Box<Detail>) -> Screen {
        let Ok(p) = &d.prepared else {
            self.error(tr!("fix the errors first", "primero corregí los errores"));
            return Screen::Detail(d);
        };
        let plan = p.plan.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = tokio::runtime::Runtime::new()
                .map_err(|e| e.to_string())
                .and_then(|rt| rt.block_on(crabfall_core::check::check(&plan)));
            let _ = tx.send(result);
        });
        Screen::Check(d, CheckView { rx, result: None })
    }

    fn start_approve(&mut self, d: Box<Detail>) -> Screen {
        let Ok(p) = &d.prepared else {
            self.error(tr!("fix the errors first", "primero corregí los errores"));
            return Screen::Detail(d);
        };
        if let Err(lines) = approve::within_limits(&p.plan, &p.env, &p.env_name) {
            self.error(lines.join(" "));
            return Screen::Detail(d);
        }
        let view = ApproveView {
            summary: d.summary.clone(),
            host: approve::target_host(&p.plan).to_string(),
            input: String::new(),
        };
        Screen::Approve(d, view)
    }

    fn on_approve(&mut self, d: Box<Detail>, mut a: ApproveView, key: KeyEvent) -> Screen {
        match key.code {
            KeyCode::Esc => return Screen::Detail(d),
            KeyCode::Backspace => {
                a.input.pop();
            }
            KeyCode::Char(c) => a.input.push(c),
            KeyCode::Enter => {
                if !a.input.trim().eq_ignore_ascii_case(&a.host) {
                    self.error(tr!(
                        "it does not match `{}`: nothing was approved",
                        "no coincide con `{}`: no se aprobó nada",
                        a.host
                    ));
                    a.input.clear();
                    return Screen::Approve(d, a);
                }
                let Ok(p) = &d.prepared else {
                    return Screen::Detail(d);
                };
                match approve::sign(&p.project, &p.plan, &p.env, &p.env_name, &p.tweaks) {
                    Ok(_) => {
                        self.info(tr!("✓ approved", "✓ aprobado"));
                        self.reload();
                        let Detail { name, form, .. } = *d;
                        return Screen::Detail(Box::new(self.open(name, form)));
                    }
                    Err(e) => {
                        self.error(e);
                        return Screen::Detail(d);
                    }
                }
            }
            _ => {}
        }
        Screen::Approve(d, a)
    }

    fn start_run(&mut self, d: Box<Detail>) -> Screen {
        let (Ok(p), Ok(approval)) = (&d.prepared, &d.approval) else {
            if let Err(lines) = &d.approval
                && !lines.is_empty()
            {
                self.error(lines[0].trim_start_matches("✗ ").to_string());
            } else {
                self.error(tr!("fix the errors first", "primero corregí los errores"));
            }
            return Screen::Detail(d);
        };
        if let Err(lines) = approve::within_limits(&p.plan, &p.env, &p.env_name) {
            self.error(lines.join(" "));
            return Screen::Detail(d);
        }
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        // El hilo arma su propio `Prepared` (no se puede compartir) con los mismos ajustes.
        let (name, env, overrides, approval) = (
            d.name.clone(),
            p.env_name.clone(),
            d.form.overrides().unwrap_or_default(),
            approval.clone(),
        );
        let thread_progress = progress.clone();
        let project = self.project.clone();
        std::thread::spawn(move || {
            let Ok(prepared) = prepare_in(project, &name, Some(&env), &[], &overrides) else {
                return;
            };
            let runtime = tokio::runtime::Runtime::new().expect("runtime de tokio");
            let finished =
                runtime.block_on(crate::run::execute(&prepared, &approval, thread_progress));
            let _ = tx.send(finished);
        });
        Screen::Running(
            d,
            RunView {
                progress,
                rx,
                ask: Ask::Nothing,
            },
        )
    }

    fn on_running(&mut self, d: Box<Detail>, mut r: RunView, key: KeyEvent) -> Screen {
        match (r.ask, key.code) {
            (Ask::Nothing, KeyCode::Char('q') | KeyCode::Esc) => {
                r.ask = if r.progress.stop_requested() {
                    Ask::Cut
                } else {
                    Ask::Stop
                }
            }
            (Ask::Stop | Ask::Cut, KeyCode::Char('y' | 's' | 'Y' | 'S') | KeyCode::Enter) => {
                r.progress.stop();
                r.ask = Ask::Nothing;
            }
            (Ask::Stop | Ask::Cut, _) => r.ask = Ask::Nothing,
            _ => {}
        }
        Screen::Running(d, r)
    }

    fn on_results(&mut self, d: Box<Detail>, mut r: Box<ResultsView>, key: KeyEvent) -> Screen {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                let Detail { name, form, .. } = *d;
                return Screen::Detail(Box::new(self.open(name, form)));
            }
            KeyCode::Right | KeyCode::Tab => r.tab = (r.tab + 1) % view::TABS,
            KeyCode::Left | KeyCode::BackTab => r.tab = (r.tab + view::TABS - 1) % view::TABS,
            KeyCode::Char(c @ '1'..='4') => r.tab = c as usize - '1' as usize,
            KeyCode::Char('h') => {
                let Detail { name, form, .. } = *d;
                let fresh = Box::new(self.open(name, form));
                return self.open_history(fresh);
            }
            KeyCode::Char('o') => match &r.finished.report {
                Some(Ok(path)) => self.open_report(path),
                _ => self.error(tr!("there is no report", "no hay reporte")),
            },
            KeyCode::Char('r') => {
                let Detail { name, form, .. } = *d;
                let fresh = Box::new(self.open(name, form));
                return self.start_run(fresh);
            }
            _ => {}
        }
        Screen::Results(d, r)
    }

    /// Las corridas de este escenario en este entorno, de la más reciente a la más vieja.
    fn open_history(&mut self, d: Box<Detail>) -> Screen {
        let env = self.env_name().to_string();
        let mut ids: Vec<String> = std::fs::read_dir(self.project.runs_dir())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.path().join("results.json").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        let rows: Vec<HistoryRow> = ids
            .into_iter()
            .filter_map(|id| {
                let file = self.project.runs_dir().join(&id).join("results.json");
                let doc = crate::runs::read(&file).ok()?;
                (doc["scenario"] == d.name.as_str() && doc["environment"] == env.as_str())
                    .then_some(HistoryRow { id, doc })
            })
            .collect();
        if rows.is_empty() {
            self.error(tr!(
                "this scenario has no runs in this environment yet: approve it (a) and run it (r)",
                "este escenario todavía no tiene corridas en este entorno: aprobalo (a) y corrélo (r)"
            ));
            return Screen::Detail(d);
        }
        let baseline = crate::baseline::run_id(&self.project, &d.name, &env);
        Screen::History(
            d,
            HistoryView {
                rows,
                baseline,
                selected: 0,
                base: None,
            },
        )
    }

    fn on_history(&mut self, d: Box<Detail>, mut h: HistoryView, key: KeyEvent) -> Screen {
        match key.code {
            // Esc primero suelta la corrida elegida; sin ninguna, vuelve.
            KeyCode::Esc if h.base.is_some() => h.base = None,
            KeyCode::Esc | KeyCode::Char('q') => return Screen::Detail(d),
            KeyCode::Up | KeyCode::Char('k') => h.selected = h.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                h.selected = (h.selected + 1).min(h.rows.len() - 1)
            }
            KeyCode::Enter | KeyCode::Char(' ') => match h.base {
                None => h.base = Some(h.selected),
                Some(b) if b == h.selected => h.base = None,
                Some(b) => return self.compare_rows(d, h, b),
            },
            KeyCode::Char('c') => {
                if h.selected + 1 < h.rows.len() {
                    let base = h.selected + 1;
                    return self.compare_rows(d, h, base);
                }
                self.error(tr!(
                    "it is the oldest run: there is no earlier one to compare with",
                    "es la corrida más vieja: no hay una anterior para comparar"
                ));
            }
            KeyCode::Char('o') => {
                let path = self
                    .project
                    .runs_dir()
                    .join(&h.rows[h.selected].id)
                    .join("report.html");
                self.open_report(&path);
            }
            KeyCode::Char('b') => {
                let id = h.rows[h.selected].id.clone();
                let results = self.project.runs_dir().join(&id).join("results.json");
                match crate::baseline::set(&self.project, &results) {
                    Ok(_) => {
                        h.baseline = Some(id.clone());
                        self.info(tr!(
                            "✓ {id} is now the baseline (crabfall compare --baseline)",
                            "✓ {id} es ahora la base (crabfall compare --baseline)"
                        ));
                    }
                    Err(e) => self.error(e),
                }
            }
            _ => {}
        }
        Screen::History(d, h)
    }

    /// Compara la corrida seleccionada contra `base` (la elegida antes, o la anterior).
    fn compare_rows(&mut self, d: Box<Detail>, mut h: HistoryView, base: usize) -> Screen {
        let parse = |row: &HistoryRow| {
            serde_json::from_value::<crabfall_core::compare::Run>(row.doc.clone())
                .map_err(|e| e.to_string())
        };
        let result = parse(&h.rows[base]).and_then(|b| {
            parse(&h.rows[h.selected]).and_then(|n| crabfall_core::compare::compare(&b, &n))
        });
        match result {
            Ok(comparison) => {
                h.base = None;
                Screen::Compare(d, h, Box::new(CompareView { comparison }))
            }
            Err(e) => {
                self.error(e);
                Screen::History(d, h)
            }
        }
    }

    fn open_report(&mut self, path: &std::path::Path) {
        if !path.is_file() {
            self.error(tr!("there is no report", "no hay reporte"));
        } else if !crate::report::open_in_browser(path) {
            self.error(tr!(
                "could not open the browser: {}",
                "no se pudo abrir el navegador: {}",
                path.display()
            ));
        }
    }

    fn error(&mut self, text: String) {
        self.message = Some((true, text));
    }

    fn info(&mut self, text: String) {
        self.message = Some((false, text));
    }
}

/// La aprobación de un escenario en un entorno, sin ajustes o con los que se aprobó.
fn status(project: &Project, name: &str, env: &str) -> Status {
    let p = match prepare_in(project.clone(), name, Some(env), &[], &Overrides::default()) {
        Ok(p) => p,
        Err(lines) => {
            let why = lines
                .get(1)
                .or(lines.first())
                .map(|l| l.trim().trim_start_matches("✗ ").to_string())
                .unwrap_or_default();
            return Status::Invalid(why);
        }
    };
    if approve::verify(project, &p.plan, &p.env, &p.env_name, &p.tweaks).is_ok() {
        return Status::Approved(vec![]);
    }
    match approve::load(project, &p.plan.scenario, &p.env_name) {
        // Se aprobó con ajustes: vale si, con esos mismos, el plan no cambió.
        Ok(Some(a)) if !a.flags.is_empty() => {
            let approved = Form::from_flags(&a.flags)
                .overrides()
                .ok()
                .and_then(|o| prepare_in(project.clone(), name, Some(env), &[], &o).ok())
                .is_some_and(|p| {
                    approve::verify(project, &p.plan, &p.env, &p.env_name, &p.tweaks).is_ok()
                });
            if approved {
                Status::Approved(a.flags)
            } else {
                Status::Changed
            }
        }
        Ok(Some(_)) => Status::Changed,
        _ => Status::NotApproved,
    }
}

/// La última corrida de cada (escenario, entorno).
fn last_runs(project: &Project) -> BTreeMap<(String, String), (String, serde_json::Value)> {
    let mut ids: Vec<String> = std::fs::read_dir(project.runs_dir())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().join("results.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    let mut out = BTreeMap::new();
    for id in ids {
        let file: PathBuf = project.runs_dir().join(&id).join("results.json");
        if let Ok(doc) = crate::runs::read(&file) {
            let key = (
                doc["scenario"].as_str().unwrap_or_default().to_string(),
                doc["environment"].as_str().unwrap_or_default().to_string(),
            );
            out.insert(key, (id, doc));
        }
    }
    out
}

/// La comparación rápida con la corrida anterior del mismo escenario y entorno.
fn comparison(project: &Project, finished: &Finished) -> Vec<String> {
    let doc = &finished.doc;
    let (Some(id), Some(scenario), Some(env)) = (
        doc["run_id"].as_str(),
        doc["scenario"].as_str(),
        doc["environment"].as_str(),
    ) else {
        return vec![];
    };
    let Ok(base_file) = crate::runs::previous(project, id, scenario, env) else {
        return vec![
            pick(
                "first run of this scenario here",
                "primera corrida de este escenario acá",
            )
            .into(),
        ];
    };
    let parse = |v: serde_json::Value| {
        serde_json::from_value::<crabfall_core::compare::Run>(v).map_err(|e| e.to_string())
    };
    let base = crate::runs::read(&base_file).and_then(parse);
    let (Ok(base), Ok(new)) = (base, parse(doc.clone())) else {
        return vec![];
    };
    match crabfall_core::compare::compare(&base, &new) {
        Ok(c) => {
            let verdict = match c.verdict {
                crabfall_core::compare::Verdict::Better => pick("better", "mejoró"),
                crabfall_core::compare::Verdict::Worse => pick("worse", "empeoró"),
                crabfall_core::compare::Verdict::Same => {
                    pick("no significant changes", "sin cambios significativos")
                }
                crabfall_core::compare::Verdict::Mixed => pick("mixed", "mixto"),
            };
            let mut out = vec![tr!(
                "against {}: {verdict}",
                "contra {}: {verdict}",
                c.base.run_id
            )];
            out.extend(c.warnings.iter().map(|w| format!("  ! {w}")));
            out.extend(c.regressions.iter().map(|r| format!("  ✗ {r}")));
            out.extend(c.improvements.iter().map(|i| format!("  ✓ {i}")));
            out
        }
        Err(e) => vec![e],
    }
}

/// Segundos desde `t`, para mostrar.
fn since(t: Option<Instant>) -> u64 {
    t.map_or(0, |t| Instant::now().saturating_duration_since(t).as_secs())
}

#[cfg(test)]
mod tests_support {
    pub use ratatui::crossterm::event::KeyCode;
    use ratatui::{Terminal, backend::TestBackend};

    pub use super::*;

    /// Una App sobre un proyecto de prueba, y la función que dibuja su pantalla.
    pub fn fixture(name: &str) -> (App, fn(&App) -> String) {
        (App::with_project(project(name)).unwrap(), screen)
    }

    pub fn project(name: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("crabfall-tui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join(".crabfall");
        std::fs::create_dir_all(root.join("scenarios")).unwrap();
        std::fs::write(
            root.join("config.yaml"),
            "version: 1\nenvironments:\n  local:\n    base_url: http://127.0.0.1:9\n    allowed_hosts: [127.0.0.1]\n    limits: { max_rps: 100 }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("scenarios/s.yaml"),
            "version: 1\nname: s\ndescription: un escenario de prueba\nflows:\n  main:\n    steps:\n      - name: a\n        request: { method: GET, url: /x }\nload: { executor: constant_rate, rate: 1/s, duration: 5s, max_vus: 2 }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("scenarios/broken.yaml"),
            "version: 1\nname: broken\n",
        )
        .unwrap();
        Project { dir: root }
    }

    pub fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
        terminal.draw(|f| view::draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer
            .content
            .chunks(width)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    pub fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }
}

#[cfg(test)]
mod tests {
    //! La TUI dibujada en un terminal en memoria. Los tests corren en español
    //! (`CRABFALL_LANG=es` en .cargo/config.toml).

    use super::tests_support::*;

    #[test]
    fn home_lists_scenarios_with_their_state() {
        let mut app = App::with_project(project("home")).unwrap();
        let s = screen(&app);
        assert!(s.contains("entorno: local"), "{s}");
        assert!(s.contains("· sin aprobar"), "{s}");
        // `broken` no tiene flows ni load: no se puede armar. Abajo va el error entero.
        let broken = s.lines().find(|l| l.contains("broken")).unwrap();
        assert!(broken.contains('✗'), "{broken}");
        assert!(s.contains("missing field `flows`"), "{s}");
        // Abajo, la descripción del escenario elegido.
        press(&mut app, KeyCode::Down);
        assert!(screen(&app).contains("un escenario de prueba"));
    }

    #[test]
    fn home_shows_the_whole_description_of_the_selected_scenario() {
        let p = project("about");
        let long = "una descripción larga que no entra en una columna, ".repeat(6) + "FIN";
        std::fs::write(
            p.dir.join("scenarios/s.yaml"),
            format!("version: 1\nname: s\ndescription: {long}\nflows:\n  main:\n    steps:\n      - name: a\n        request: {{ method: GET, url: /x }}\nload: {{ executor: constant_rate, rate: 1/s, duration: 5s, max_vus: 2 }}\n"),
        )
        .unwrap();
        let mut app = App::with_project(p).unwrap();
        press(&mut app, KeyCode::Char('j'));
        let s = screen(&app);
        assert!(s.contains("FIN"), "{s}");
        assert!(s.contains("Enter para abrirlo"), "{s}");
        assert!(s.contains("? ayuda"), "{s}");
    }

    #[test]
    fn question_mark_opens_the_help_and_any_key_closes_it() {
        let mut app = App::with_project(project("help")).unwrap();
        press(&mut app, KeyCode::Char('?'));
        let s = screen(&app);
        assert!(s.contains("Ayuda · escenarios"), "{s}");
        // La tecla que cierra la ayuda no hace nada más: no sale.
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.quit);
        assert!(!screen(&app).contains("Ayuda · escenarios"));

        // Editando un ajuste, ? se escribe.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('?'));
        assert!(!app.help);
        let Screen::Detail(d) = &app.screen else {
            panic!("no se abrió el escenario")
        };
        assert_eq!(d.form.fields[0].value, "?");
    }

    #[test]
    fn the_scenario_says_what_comes_next() {
        let mut app = App::with_project(project("next")).unwrap();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let s = screen(&app);
        assert!(s.contains("① check (c)"), "{s}");
        assert!(s.contains("Siguiente: c para probar cada paso"), "{s}");
        // Lo que significa el ajuste elegido.
        assert!(s.contains("Modelo de carga"), "{s}");

        // Un check que falló lo dice; después de él, sigue aprobar.
        app.checks.insert("s".into(), false);
        assert!(screen(&app).contains("el check falló"));
        app.checks.insert("s".into(), true);
        let s = screen(&app);
        assert!(s.contains("Siguiente: a para aprobar"), "{s}");

        // Un ajuste que no se entiende va primero.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "abc");
        press(&mut app, KeyCode::Enter);
        let s = screen(&app);
        assert!(s.contains("el ajuste `rate` no se entiende"), "{s}");
    }

    #[test]
    fn tweaks_rebuild_the_plan_and_show_their_errors() {
        let mut app = App::with_project(project("tweaks")).unwrap();
        // `broken` va primero (orden alfabético): bajar a `s` y abrirlo.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let s = screen(&app);
        assert!(s.contains("Escenario   s"), "{s}");
        assert!(s.contains("constant_rate 1/s"), "{s}");

        // rate es el segundo ajuste.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "abc");
        press(&mut app, KeyCode::Enter);
        let s = screen(&app);
        assert!(s.contains("tasa inválida `abc`"), "{s}");

        press(&mut app, KeyCode::Delete);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "5/s");
        press(&mut app, KeyCode::Enter);
        let s = screen(&app);
        assert!(s.contains("constant_rate 5/s"), "{s}");
        assert!(
            s.contains("crabfall approve s --env local --rate 5/s"),
            "{s}"
        );
    }

    #[test]
    fn running_needs_an_approval_and_approving_needs_the_host() {
        let mut app = App::with_project(project("approve")).unwrap();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.screen, Screen::Detail(_)));
        let (_, message) = app.message.clone().expect("aviso");
        assert!(message.contains("no está aprobado"), "{message}");

        press(&mut app, KeyCode::Char('a'));
        let s = screen(&app);
        assert!(s.contains("escribí el host destino (127.0.0.1)"), "{s}");
        type_text(&mut app, "otro.host");
        press(&mut app, KeyCode::Enter);
        let (_, message) = app.message.clone().expect("aviso");
        assert!(message.contains("no coincide"), "{message}");
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.screen, Screen::Detail(_)));
    }

    #[test]
    fn check_runs_in_the_background() {
        let mut app = App::with_project(project("check")).unwrap();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('c'));
        // Nadie escucha en 127.0.0.1:9: el paso falla, y la pantalla lo muestra.
        for _ in 0..100 {
            app.poll_background();
            if matches!(&app.screen, Screen::Check(_, c) if c.result.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let s = screen(&app);
        assert!(s.contains("✗ main/a"), "{s}");
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.screen, Screen::Detail(_)));
    }

    #[test]
    fn the_run_screen_draws_before_any_data() {
        let mut app = App::with_project(project("running")).unwrap();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let Screen::Detail(d) = std::mem::replace(&mut app.screen, Screen::Home) else {
            panic!("no se abrió el escenario")
        };
        let (_tx, rx) = mpsc::channel();
        app.screen = Screen::Running(
            d,
            RunView {
                progress: Arc::new(Progress::default()),
                rx,
                ask: Ask::Nothing,
            },
        );
        let s = screen(&app);
        assert!(s.contains("q frenar"), "{s}");
        press(&mut app, KeyCode::Char('q'));
        let s = screen(&app);
        assert!(s.contains("¿Frenar?"), "{s}");
    }
}

#[cfg(test)]
mod look {
    //! `cargo test -p crabfall-cli --bin crabfall look -- --ignored --nocapture` imprime las
    //! pantallas, para mirarlas.

    use super::tests_support::*;

    #[test]
    #[ignore = "imprime pantallas para mirarlas"]
    fn print_screens() {
        let (mut app, screen) = fixture("look");
        println!("{}\n", screen(&app));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        println!("{}\n", screen(&app));
        press(&mut app, KeyCode::Char('?'));
        println!("{}\n", screen(&app));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('a'));
        println!("{}", screen(&app));
    }
}

#[cfg(test)]
mod look_live {
    //! `cargo test -p crabfall-cli --bin crabfall look_live -- --ignored --nocapture`: una
    //! corrida real contra un servidor mínimo, dibujada en vivo y después en resultados.

    use std::io::{Read, Write};

    use super::tests_support::*;

    /// Responde `ok` a todo, con keep-alive.
    fn server() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut buf = [0; 4096];
                    while let Ok(n) = stream.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
                    }
                });
            }
        });
        port
    }

    #[test]
    #[ignore = "imprime pantallas para mirarlas"]
    fn print_run_and_results() {
        let port = server();
        let p = project("look-live");
        std::fs::write(
            p.dir.join("config.yaml"),
            format!("version: 1\nenvironments:\n  local:\n    base_url: http://127.0.0.1:{port}\n    allowed_hosts: [127.0.0.1]\n"),
        )
        .unwrap();
        let mut app = App::with_project(p).unwrap();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let Screen::Detail(d) = std::mem::replace(&mut app.screen, Screen::Home) else {
            panic!()
        };
        let plan = d.prepared.as_ref().unwrap().plan.clone();
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let thread_progress = progress.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(crabfall_core::engine::run(plan, thread_progress));
            let _ = tx.send(Finished {
                result,
                doc: serde_json::json!({}),
                results: Err("no se guarda en este test".into()),
                report: None,
            });
        });
        app.screen = Screen::Running(
            d,
            RunView {
                progress,
                rx,
                ask: Ask::Nothing,
            },
        );
        std::thread::sleep(Duration::from_millis(3200));
        println!("{}\n", screen(&app));
        for _ in 0..200 {
            app.poll_background();
            if matches!(app.screen, Screen::Results(..)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        for tab in ['1', '3'] {
            press(&mut app, KeyCode::Char(tab));
            println!("{}\n", screen(&app));
        }
    }
}

#[cfg(test)]
mod history_tests {
    use super::tests_support::*;

    /// Una corrida guardada del escenario `s` con un p95 constante por segundo.
    fn save_run(project: &Project, id: &str, p95: f64) {
        let points: Vec<serde_json::Value> = (0..8)
            .map(|t| serde_json::json!({ "t": t, "requests": 10, "errors": 0, "p50": p95 / 2.0, "p95": p95, "p99": p95 }))
            .collect();
        let global: Vec<serde_json::Value> = (0..8)
            .map(|t| serde_json::json!({ "t": t, "started": 10, "dropped": 0, "requests": 10, "errors": 0, "p50": 1.0, "p95": 1.0, "p99": 1.0 }))
            .collect();
        let doc = serde_json::json!({
            "run_id": id, "scenario": "s", "environment": "local", "passed": true, "duration_s": 8.0,
            "load": { "executor": "constant_rate", "rate": "10/s" },
            "totals": { "planned": 80, "started": 80, "dropped": 0, "interrupted": 0, "requests": 80,
                        "failed": 0, "non_2xx": 0, "checks_passed": 0, "checks_failed": 0,
                        "iteration_rate": 10.0, "request_rate": 10.0 },
            "steps": [{ "name": "a", "requests": 80, "failed": 0, "non_2xx": 0,
                        "latency_ms": { "count": 80, "mean": p95, "p50": p95 / 2.0, "p90": p95, "p95": p95, "p99": p95, "p999": p95, "max": p95 } }],
            "findings": [],
            "series": { "window_s": 1, "points": global, "steps": [{ "name": "a", "points": points }] },
        });
        let dir = project.runs_dir().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("results.json"), doc.to_string()).unwrap();
    }

    #[test]
    fn history_lists_runs_and_compares_them() {
        let p = project("history");
        save_run(&p, "20261020-100000", 10.0);
        save_run(&p, "20261020-110000", 40.0);
        let mut app = App::with_project(p).unwrap();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('h'));
        let s = screen(&app);
        // La más reciente primero.
        let newer = s.find("20261020-110000").expect("corrida nueva");
        let older = s.find("20261020-100000").expect("corrida vieja");
        assert!(newer < older, "{s}");

        // c: la seleccionada (la más reciente) contra la anterior.
        press(&mut app, KeyCode::Char('c'));
        let s = screen(&app);
        assert!(s.contains("20261020-100000 → 20261020-110000"), "{s}");
        assert!(s.contains("empeoró"), "{s}");

        // Enter elige la primera; Enter en otra las compara: la elegida es el "antes".
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Enter);
        let s = screen(&app);
        assert!(s.contains("◆ elegida"), "{s}");
        assert!(s.contains("Enter en otra corrida: compararlas"), "{s}");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let s = screen(&app);
        assert!(s.contains("20261020-110000 → 20261020-100000"), "{s}");
        assert!(s.contains("mejoró"), "{s}");

        // Al volver, no queda ninguna elegida; Enter y Esc la sueltan sin salir.
        press(&mut app, KeyCode::Esc);
        assert!(!screen(&app).contains("◆ elegida"));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.screen, Screen::History(..)));
        assert!(!screen(&app).contains("◆ elegida"));
    }

    #[test]
    fn b_sets_the_baseline_from_the_history() {
        let p = project("baseline");
        save_run(&p, "20261020-100000", 10.0);
        save_run(&p, "20261020-110000", 40.0);
        let baselines = p.dir.join("baselines/s.local.json");
        let mut app = App::with_project(p).unwrap();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('h'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('b'));
        assert!(baselines.is_file());
        let s = screen(&app);
        let row = s.lines().find(|l| l.contains("20261020-100000")).unwrap();
        assert!(row.contains("base fija"), "{row}");
    }

    #[test]
    fn history_without_runs_says_so() {
        let mut app = App::with_project(project("no-history")).unwrap();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('h'));
        assert!(matches!(app.screen, Screen::Detail(_)));
        let (_, message) = app.message.clone().expect("aviso");
        assert!(message.contains("todavía no tiene corridas"), "{message}");
    }
}
