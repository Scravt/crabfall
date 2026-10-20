//! Plan ejecutable: escenario validado + entorno resuelto.
//!
//! Los pasos guardan plantillas (`template`) que se resuelven en cada request. El host
//! de cada paso es fijo: se valida contra `allowed_hosts` al armar el plan.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
    time::Duration,
};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    Method, Request,
    header::{ACCEPT_ENCODING, CONTENT_TYPE, HOST, HeaderName, HeaderValue, USER_AGENT},
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use url::{Url, form_urlencoded};

use super::http::{Body, Origin};
use crate::{
    assertion::Assertion,
    config::Environment,
    data::Dataset,
    extract::Extractor,
    load::LoadPlan,
    scenario::{Defaults, MultipartPart, Pick, Scenario, Source, Step},
    template::{Scope, Template, ValueTemplate, to_text},
    threshold::{Filter, ThresholdPlan},
    units::{HumanDuration, Think},
    upload::{self, Upload},
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Redirects que sigue un paso si el escenario no dice otra cosa.
pub const DEFAULT_REDIRECTS: u32 = 10;

/// Lo que piden los clientes reales; un header `accept-encoding` del escenario lo pisa.
pub const DEFAULT_ACCEPT_ENCODING: &str = "gzip, br";

/// Lo que se codifica de un valor interpolado en el path: todo menos lo no reservado.
const PATH_VALUE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Entradas que no vienen del escenario ni del entorno.
#[derive(Debug, Default, Clone)]
pub struct Inputs {
    /// `--var nombre=valor`.
    pub vars: BTreeMap<String, Value>,
    /// Secretos declarados en `config.yaml`, con su valor si lo tienen.
    pub secrets: BTreeMap<String, Option<String>>,
    /// Datasets de `data`, ya leídos.
    pub data: BTreeMap<String, Dataset>,
    /// Archivos de los bodies multipart, ya leídos, por su ruta en el escenario.
    pub files: BTreeMap<String, Upload>,
}

pub struct Plan {
    pub scenario: String,
    /// Pasos que corren una vez antes de la carga.
    pub setup: Vec<StepPlan>,
    /// Pasos que cada VU corre una vez antes de la carga.
    pub vu_setup: Vec<StepPlan>,
    /// Renovación de la sesión de cada VU (`vu_refresh`).
    pub vu_refresh: Option<RefreshPlan>,
    /// Índices en `data` de los datasets que usan `vu_setup` y `vu_refresh` (una fila por VU).
    pub vu_data: Vec<usize>,
    pub flows: Vec<FlowPlan>,
    pub load: LoadPlan,
    /// Hosts a los que se va a conectar.
    pub hosts: BTreeSet<String>,
    /// `allowed_hosts` del entorno, en minúsculas: un redirect solo se sigue hacia uno de estos.
    pub allowed_hosts: BTreeSet<String>,
    /// Cada VU guarda y reenvía sus cookies (`cookies`, por defecto `true`).
    pub cookies: bool,
    /// Cota de requests/s; `None` con `constant_vus`.
    pub max_request_rate: Option<f64>,
    /// `limits.max_rps` del entorno: el motor no lo supera aunque la tasa no esté acotada.
    pub request_cap: Option<f64>,
    /// Variables globales: escenario < entorno < `--var`.
    pub vars: BTreeMap<String, Value>,
    /// Datasets de `data`, en el orden del escenario.
    pub data: Vec<DataPlan>,
    /// Valores de los secretos usados. Nunca se muestran ni se firman.
    pub secrets: HashMap<String, String>,
    /// No firman: no cambian el tráfico.
    pub thresholds: Vec<ThresholdPlan>,
}

pub struct FlowPlan {
    pub name: String,
    pub weight: u32,
    pub steps: Vec<StepPlan>,
    /// Índices en `Plan::data` de los datasets que usan sus pasos.
    pub data: Vec<usize>,
}

/// `vu_refresh`: cada cuánto y con qué pasos. Sin pasos propios, se repite `vu_setup`.
pub struct RefreshPlan {
    pub every: Duration,
    pub steps: Vec<StepPlan>,
}

/// Un dataset con su contador de filas.
pub struct DataPlan {
    pub dataset: Dataset,
    next: AtomicUsize,
}

impl DataPlan {
    /// Fila de la próxima iteración.
    fn pick(&self) -> usize {
        let rows = self.dataset.rows.len();
        match self.dataset.pick {
            Pick::Sequential => self.next.fetch_add(1, Relaxed) % rows,
            Pick::Random => fastrand::usize(..rows),
        }
    }
}

pub enum BodyPlan {
    None,
    Json {
        template: ValueTemplate,
        source: Value,
    },
    Text(Template),
    /// `application/x-www-form-urlencoded`: campo y valor, en el orden del escenario.
    Form(Vec<(String, Template)>),
    /// `multipart/form-data`. El boundary es fijo por paso y no se firma: no está en el escenario.
    Multipart {
        boundary: String,
        parts: Vec<(String, PartPlan)>,
    },
}

pub enum PartPlan {
    Text(Template),
    File {
        upload: Upload,
        filename: Template,
        content_type: String,
    },
}

enum QueryPart {
    /// Query escrita en la URL (`?a=1&b={{x}}`): lo literal va tal cual.
    Raw(Template),
    Pair(String, Template),
}

pub struct StepPlan {
    /// Índice global del paso: clave de las métricas.
    pub id: usize,
    pub name: String,
    pub flow: String,
    pub tags: Vec<String>,
    pub source: Option<Source>,
    pub method: Method,
    pub origin: Origin,
    path: Template,
    query: Vec<QueryPart>,
    headers: Vec<(HeaderName, Template)>,
    pub body: BodyPlan,
    pub timeout: Duration,
    /// Cuántos redirects sigue (`0`: ninguno).
    pub redirects: u32,
    /// Pausa después del paso; `None` si no hay.
    pub think: Option<Think>,
    /// Variables que salen de la respuesta.
    pub extract: Vec<(String, Extractor)>,
    /// Verificaciones de la respuesta.
    pub checks: Vec<Assertion>,
}

impl StepPlan {
    /// Path + query resueltos, para la línea de request.
    pub fn target(&self, scope: &dyn Scope) -> Result<String, String> {
        let mut target = self
            .path
            .render_with(scope, |s| utf8_percent_encode(s, PATH_VALUE).to_string())?;
        let mut query = Vec::new();
        for part in &self.query {
            query.push(match part {
                QueryPart::Raw(t) => t.render_with(scope, form_encode)?,
                QueryPart::Pair(k, v) => {
                    format!("{}={}", form_encode(k), form_encode(&v.render(scope)?))
                }
            });
        }
        if !query.is_empty() {
            target.push('?');
            target.push_str(&query.join("&"));
        }
        Ok(target)
    }

    pub fn request(&self, scope: &dyn Scope) -> Result<Request<Body>, String> {
        let body = match &self.body {
            BodyPlan::None => Bytes::new(),
            BodyPlan::Json { template, .. } => Bytes::from(
                serde_json::to_vec(&template.render(scope)?).expect("JSON siempre serializa"),
            ),
            BodyPlan::Text(t) => Bytes::from(t.render(scope)?),
            BodyPlan::Form(fields) => {
                let mut form = form_urlencoded::Serializer::new(String::new());
                for (name, value) in fields {
                    form.append_pair(name, &value.render(scope)?);
                }
                Bytes::from(form.finish())
            }
            BodyPlan::Multipart { boundary, parts } => multipart(boundary, parts, scope)?,
        };
        let mut request = Request::builder()
            .method(self.method.clone())
            .uri(self.target(scope)?)
            .body(Full::new(body))
            .map_err(|e| crate::tr!("invalid request: {e}", "request inválido: {e}"))?;
        for (name, value) in &self.headers {
            let value = HeaderValue::from_str(&value.render(scope)?).map_err(|e| {
                crate::tr!(
                    "invalid header `{name}`: {e}",
                    "header `{name}` inválido: {e}"
                )
            })?;
            request.headers_mut().insert(name.clone(), value);
        }
        if let BodyPlan::Multipart { boundary, .. } = &self.body {
            let value = format!("multipart/form-data; boundary={boundary}");
            request.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_str(&value).expect("el boundary es ASCII"),
            );
        }
        Ok(request)
    }

    /// URL con las plantillas sin resolver, para mostrar y firmar.
    pub fn url(&self) -> String {
        let scheme = if self.origin.https { "https" } else { "http" };
        let mut url = format!(
            "{scheme}://{}:{}{}",
            self.origin.host,
            self.origin.port,
            self.path.source()
        );
        let query: Vec<String> = self
            .query
            .iter()
            .map(|p| match p {
                QueryPart::Raw(t) => t.source().to_string(),
                QueryPart::Pair(k, v) => format!("{k}={}", v.source()),
            })
            .collect();
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        url
    }

    /// Si alguna plantilla o check usa una columna de `dataset`.
    fn uses_dataset(&self, dataset: &str) -> bool {
        let is_column = |var: &str| var.split_once('.').is_some_and(|(d, _)| d == dataset);
        self.templates().iter().any(|t| t.vars().any(is_column))
            || self
                .checks
                .iter()
                .any(|c| c.vars().into_iter().any(is_column))
    }

    /// Todas las plantillas del paso.
    fn templates(&self) -> Vec<&Template> {
        let mut all = vec![&self.path];
        all.extend(self.query.iter().map(|p| match p {
            QueryPart::Raw(t) | QueryPart::Pair(_, t) => t,
        }));
        all.extend(self.headers.iter().map(|(_, t)| t));
        match &self.body {
            BodyPlan::None => {}
            BodyPlan::Json { template, .. } => all.extend(template.templates()),
            BodyPlan::Text(t) => all.push(t),
            BodyPlan::Form(fields) => all.extend(fields.iter().map(|(_, t)| t)),
            BodyPlan::Multipart { parts, .. } => all.extend(parts.iter().map(|(_, p)| match p {
                PartPlan::Text(t) => t,
                PartPlan::File { filename, .. } => filename,
            })),
        }
        all
    }
}

fn form_encode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

impl Plan {
    /// Arma el plan: resuelve URLs, verifica `allowed_hosts`, parsea las plantillas y
    /// verifica que toda variable y todo secreto usados existan. No aplica los límites
    /// (ver `limit_violations`). Requiere un escenario que pasó `validate`.
    pub fn build(
        scenario: &Scenario,
        env: &Environment,
        inputs: &Inputs,
    ) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        let base = Url::parse(&env.base_url).map_err(|e| {
            vec![crate::tr!(
                "invalid base_url `{}`: {e}",
                "base_url `{}` inválida: {e}",
                env.base_url
            )]
        })?;
        let allowed: BTreeSet<String> = env
            .allowed_hosts
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();

        let mut vars: BTreeMap<String, Value> = scenario.vars.clone().into_iter().collect();
        vars.extend(env.vars.clone());
        vars.extend(inputs.vars.clone());
        let mut hosts = BTreeSet::new();
        // `id` indexa las métricas de los flows; `setup` lleva su propia numeración.
        let mut flow_ids = 0;
        let mut setup_ids = 0;
        let columns: BTreeMap<&str, &[String]> = scenario
            .data
            .keys()
            .filter_map(|n| {
                inputs
                    .data
                    .get(n)
                    .map(|d| (n.as_str(), d.columns.as_slice()))
            })
            .collect();
        // Arma los pasos en orden; cada uno ve las variables globales y las extraídas antes.
        let mut build_steps = |steps: &[Step],
                               flow: &str,
                               defaults: Option<&Defaults>,
                               known: &mut BTreeSet<String>,
                               id: &mut usize| {
            let mut out = Vec::new();
            for step in steps {
                match build_step(*id, flow, step, defaults, &base, &allowed, &inputs.files) {
                    Ok(plan) => {
                        let problems = check_references(&plan, known, &inputs.secrets, &columns);
                        errors.extend(problems.into_iter().map(|p| format!("{}: {p}", step.name)));
                        known.extend(plan.extract.iter().map(|(name, _)| name.clone()));
                        hosts.insert(plan.origin.host.clone());
                        out.push(plan);
                    }
                    Err(e) => errors.push(format!("{}: {e}", step.name)),
                }
                *id += 1;
            }
            out
        };

        let mut after_setup: BTreeSet<String> = vars.keys().cloned().collect();
        let setup = build_steps(
            &scenario.setup,
            "setup",
            None,
            &mut after_setup,
            &mut setup_ids,
        );
        // Las columnas de los datasets existen en `vu_setup` y en los flows, no en `setup`.
        let mut data = Vec::new();
        let mut data_errors = Vec::new();
        for name in scenario.data.keys() {
            match inputs.data.get(name) {
                Some(dataset) => {
                    after_setup.extend(dataset.vars(0).map(|(var, _)| var));
                    data.push(DataPlan {
                        dataset: dataset.clone(),
                        next: AtomicUsize::new(0),
                    });
                }
                None => data_errors.push(crate::tr!(
                    "data.{name}: the dataset was not loaded",
                    "data.{name}: el dataset no se cargó"
                )),
            }
        }
        // Lo que extrae `vu_setup` lo ven los flows: se suma a `after_setup`.
        let mut vu_ids = 0;
        let vu_setup = build_steps(
            &scenario.vu_setup,
            "vu_setup",
            None,
            &mut after_setup,
            &mut vu_ids,
        );
        // La renovación ve lo mismo que `vu_setup`. Lo que extrae no se suma a lo que ven los
        // flows: una variable que solo existiera después de renovar faltaría al principio.
        let mut refresh_ids = 0;
        let vu_refresh = scenario.vu_refresh.as_ref().map(|r| RefreshPlan {
            every: r.every.0,
            steps: build_steps(
                &r.steps,
                "vu_refresh",
                None,
                &mut after_setup.clone(),
                &mut refresh_ids,
            ),
        });
        let vu_steps: Vec<&StepPlan> = vu_setup
            .iter()
            .chain(vu_refresh.iter().flat_map(|r| &r.steps))
            .collect();
        let vu_data = datasets_used(&data, vu_steps.iter().copied());
        let flows: Vec<FlowPlan> = scenario
            .flows
            .iter()
            .map(|(flow_name, flow)| {
                let steps = build_steps(
                    &flow.steps,
                    flow_name,
                    scenario.defaults.as_ref(),
                    &mut after_setup.clone(),
                    &mut flow_ids,
                );
                // Un dataset de `vu_setup` es del VU: los flows ven su fila, no toman otra.
                let data = datasets_used(&data, &steps)
                    .into_iter()
                    .filter(|i| !vu_data.contains(i))
                    .collect();
                FlowPlan {
                    name: flow_name.clone(),
                    weight: flow.weight,
                    steps,
                    data,
                }
            })
            .collect();

        let secrets = inputs
            .secrets
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| (k.clone(), v.clone())))
            .collect();
        let plan = Self {
            scenario: scenario.name.clone(),
            setup,
            vu_setup,
            vu_refresh,
            vu_data,
            flows,
            load: scenario.load.plan(),
            hosts,
            allowed_hosts: allowed.clone(),
            cookies: scenario.cookies.unwrap_or(true),
            max_request_rate: scenario.max_request_rate(),
            request_cap: env.limits.max_rps,
            vars,
            data,
            secrets,
            thresholds: Vec::new(),
        };
        errors.extend(data_errors);
        let mut plan = plan;
        for (i, t) in scenario.thresholds.iter().enumerate() {
            match ThresholdPlan::parse(t) {
                Ok(t) => {
                    let exists = match &t.filter {
                        Filter::All => true,
                        Filter::Step(s) => plan.steps().any(|p| &p.name == s),
                        Filter::Tag(tag) => plan.steps().any(|p| p.tags.contains(tag)),
                    };
                    if exists {
                        plan.thresholds.push(t);
                    } else {
                        errors.push(crate::tr!("thresholds[{i}]: no flow step matches the filter of `{}`", "thresholds[{i}]: ningún paso de los flows coincide con el filtro de `{}`", t.label));
                    }
                }
                Err(e) => errors.push(format!("thresholds[{i}]: {e}")),
            }
        }
        if errors.is_empty() {
            Ok(plan)
        } else {
            Err(errors)
        }
    }

    /// Representación canónica de todo lo que define el tráfico: es lo que firma una
    /// aprobación (ADR-004 §1). Incluye el entorno resuelto y las variables; excluye
    /// `description`, `meta`, `source` y los valores de los secretos. El orden es fijo.
    pub fn canonical(&self, env: &Environment) -> Value {
        let d = |d: Duration| HumanDuration(d).to_string();
        let mut allowed: Vec<String> = env
            .allowed_hosts
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        allowed.sort();
        allowed.dedup();
        let step = |s: &StepPlan| {
            let mut headers: Vec<(String, String)> = s
                .headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.source().to_string()))
                .collect();
            headers.sort();
            let body = match &s.body {
                BodyPlan::None => Value::Null,
                BodyPlan::Json { source, .. } => json!({ "json": source }),
                BodyPlan::Text(t) => json!({ "text": t.source() }),
                BodyPlan::Form(fields) => json!({
                    "form": fields.iter().map(|(n, v)| json!([n, v.source()])).collect::<Vec<_>>()
                }),
                // Se firma el contenido de cada archivo, no solo su ruta (ADR-012).
                BodyPlan::Multipart { parts, .. } => json!({
                    "multipart": parts.iter().map(|(name, p)| match p {
                        PartPlan::Text(t) => json!([name, { "text": t.source() }]),
                        PartPlan::File { upload, filename, content_type } => json!([name, {
                            "file": upload.file,
                            "sha256": upload.sha256,
                            "filename": filename.source(),
                            "content_type": content_type,
                        }]),
                    }).collect::<Vec<_>>()
                }),
            };
            let extract: Vec<Value> = s
                .extract
                .iter()
                .map(|(name, e)| json!([name, e.source()]))
                .collect();
            let mut step = json!({
                "name": s.name,
                "method": s.method.as_str(),
                "url": s.url(),
                "headers": headers,
                "body": body,
                "timeout": d(s.timeout),
                "tags": s.tags,
                "extract": extract,
            });
            // Solo si no es el de siempre (ADR-014).
            if s.redirects != DEFAULT_REDIRECTS {
                step["redirects"] = json!(s.redirects);
            }
            // Solo si hay pausa, para no invalidar las aprobaciones de antes (ADR-008).
            if let Some(think) = s.think {
                step["think"] = json!(think.to_string());
            }
            step
        };
        let setup: Vec<Value> = self.setup.iter().map(step).collect();
        let flows: Vec<Value> = self
            .flows
            .iter()
            .map(|f| json!({ "name": f.name, "weight": f.weight, "steps": f.steps.iter().map(step).collect::<Vec<_>>() }))
            .collect();
        let mut canonical = json!({
            "scenario": self.scenario,
            "environment": {
                "base_url": env.base_url,
                "allowed_hosts": allowed,
                "limits": {
                    "max_rps": env.limits.max_rps,
                    "max_vus": env.limits.max_vus,
                    "max_duration": env.limits.max_duration.map(|m| m.to_string()),
                },
            },
            "vars": self.vars.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
            "load": self.load.canonical(),
            "setup": setup,
            "flows": flows,
        });
        // Solo si se apagaron (ADR-014).
        if !self.cookies {
            canonical["cookies"] = json!(false);
        }
        // Solo si hay `vu_setup`, para no invalidar las aprobaciones de antes (ADR-010).
        if !self.vu_setup.is_empty() {
            canonical["vu_setup"] = self.vu_setup.iter().map(step).collect();
        }
        // Solo si hay `vu_refresh` (ADR-011).
        if let Some(r) = &self.vu_refresh {
            canonical["vu_refresh"] = json!({
                "every": d(r.every),
                "steps": r.steps.iter().map(step).collect::<Vec<_>>(),
            });
        }
        // Solo si hay datasets, para no invalidar las aprobaciones de antes (ADR-007).
        // Se firma el contenido del archivo, no solo su nombre.
        if !self.data.is_empty() {
            canonical["data"] = self
                .data
                .iter()
                .map(|d| {
                    let d = &d.dataset;
                    json!({ "name": d.name, "file": d.file, "pick": d.pick.as_str(), "sha256": d.sha256 })
                })
                .collect();
        }
        canonical
    }

    /// Variables de las filas que toma una iteración de `flow`: `dataset.columna` → valor.
    pub fn row_vars(&self, flow: &FlowPlan) -> HashMap<String, Value> {
        self.rows(&flow.data)
    }

    /// Archivos que sube cada paso con multipart: `files/foto.jpg (120 KB) en upload`.
    pub fn uploads_summary(&self) -> Vec<String> {
        self.all_steps()
            .flat_map(|s| match &s.body {
                BodyPlan::Multipart { parts, .. } => parts
                    .iter()
                    .filter_map(|(_, p)| match p {
                        PartPlan::File { upload, .. } => Some(crate::tr!(
                            "{} in {}",
                            "{} en {}",
                            upload.describe(),
                            s.name
                        )),
                        PartPlan::Text(_) => None,
                    })
                    .collect(),
                _ => vec![],
            })
            .collect()
    }

    /// Cómo se comporta el cliente, para quien aprueba: `cookies por VU · sigue hasta 10
    /// redirects (solo a allowed_hosts) · acepta gzip, br`.
    pub fn http_summary(&self) -> String {
        let cookies = if self.cookies {
            crate::tr!("cookies per VU", "cookies por VU")
        } else {
            crate::tr!("no cookies", "sin cookies")
        };
        let mut limits: Vec<u32> = self.all_steps().map(|s| s.redirects).collect();
        limits.sort_unstable();
        limits.dedup();
        let redirects = match limits.as_slice() {
            [0] => crate::tr!("does not follow redirects", "no sigue redirects").to_string(),
            [n] => crate::tr!(
                "follows up to {n} redirects (only to allowed_hosts)",
                "sigue hasta {n} redirects (solo a allowed_hosts)"
            ),
            _ => crate::tr!(
                "follows redirects per step (only to allowed_hosts)",
                "sigue redirects según el paso (solo a allowed_hosts)"
            )
            .to_string(),
        };
        let mut encodings: Vec<String> = self
            .all_steps()
            .filter_map(|s| {
                s.headers
                    .iter()
                    .find(|(n, _)| *n == ACCEPT_ENCODING)
                    .map(|(_, v)| v.source().to_string())
            })
            .collect();
        encodings.sort();
        encodings.dedup();
        let compression = match encodings.as_slice() {
            [e] => crate::tr!("accepts {e}", "acepta {e}"),
            _ => crate::tr!(
                "accepts compression per step",
                "acepta compresión según el paso"
            )
            .to_string(),
        };
        format!("{cookies} · {redirects} · {compression}")
    }

    /// Pasos que renuevan la sesión de un VU: los de `vu_refresh` o, si no tiene, `vu_setup`.
    /// Vacío sin `vu_refresh`.
    pub fn refresh_steps(&self) -> &[StepPlan] {
        match &self.vu_refresh {
            None => &[],
            Some(r) if r.steps.is_empty() => &self.vu_setup,
            Some(r) => &r.steps,
        }
    }

    /// `vu_refresh` para los resúmenes: `refresh cada 4m` o `repite login cada 4m`.
    pub fn refresh_summary(&self) -> Option<String> {
        let r = self.vu_refresh.as_ref()?;
        let names: Vec<&str> = self
            .refresh_steps()
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        let what = if r.steps.is_empty() {
            crate::tr!("repeats {}", "repite {}", names.join(", "))
        } else {
            names.join(", ")
        };
        Some(crate::tr!(
            "{what} every {}",
            "{what} cada {}",
            HumanDuration(r.every)
        ))
    }

    /// Variables de las filas que toma un VU para su `vu_setup`.
    pub fn vu_row_vars(&self) -> HashMap<String, Value> {
        self.rows(&self.vu_data)
    }

    fn rows(&self, datasets: &[usize]) -> HashMap<String, Value> {
        datasets
            .iter()
            .flat_map(|&i| {
                let d = &self.data[i];
                d.dataset.vars(d.pick())
            })
            .collect()
    }

    /// Cuánta carga puede generar, en requests/s, para resúmenes.
    pub fn rate_summary(&self) -> String {
        match (self.max_request_rate, self.request_cap) {
            (Some(rate), _) => {
                crate::tr!("up to {rate:.1} requests/s", "hasta {rate:.1} requests/s")
            }
            (None, Some(cap)) => {
                crate::tr!(
                    "depends on latency, capped at {cap} requests/s (limits.max_rps)",
                    "según la latencia, con tope de {cap} requests/s (limits.max_rps)"
                )
            }
            (None, None) => crate::tr!(
                "depends on latency, uncapped (set limits.max_rps in the environment to cap it)",
                "según la latencia, sin tope (definí limits.max_rps en el entorno para acotarla)"
            ),
        }
    }

    /// Pausas de los flows para los resúmenes: `1s-3s después de cada paso`, o paso por paso
    /// si no son todas iguales. `None` si no hay pausas.
    pub fn think_summary(&self) -> Option<String> {
        let thinks: Vec<_> = self.steps().map(|s| (s.name.as_str(), s.think)).collect();
        if thinks.iter().all(|(_, t)| t.is_none()) {
            return None;
        }
        let first = thinks[0].1;
        if thinks.iter().all(|(_, t)| *t == first) {
            return first.map(|t| crate::tr!("{t} after each step", "{t} después de cada paso"));
        }
        let each: Vec<String> = thinks
            .iter()
            .filter_map(|(name, t)| t.map(|t| format!("{name} {t}")))
            .collect();
        Some(crate::tr!("after {}", "después de {}", each.join(", ")))
    }

    /// Pasos de los flows (los que generan carga y tienen métricas).
    pub fn steps(&self) -> impl Iterator<Item = &StepPlan> {
        self.flows.iter().flat_map(|f| &f.steps)
    }

    /// `setup` y flows: todo lo que se envía.
    pub fn all_steps(&self) -> impl Iterator<Item = &StepPlan> {
        self.setup
            .iter()
            .chain(&self.vu_setup)
            .chain(self.vu_refresh.iter().flat_map(|r| &r.steps))
            .chain(self.steps())
    }

    /// Límites duros del entorno (ADR-004). `run` los exige; `check` no los necesita
    /// porque ignora `load`.
    pub fn limit_violations(&self, env: &Environment) -> Vec<String> {
        let limits = &env.limits;
        let mut out = Vec::new();
        // Con `constant_vus` la tasa no está acotada de antemano: el motor la frena a `max_rps`.
        if let (Some(max), Some(rate)) = (limits.max_rps, self.max_request_rate)
            && rate > max
        {
            out.push(crate::tr!(
                "the maximum request rate ({rate:.1}/s) exceeds limits.max_rps ({max})",
                "la tasa máxima de requests ({rate:.1}/s) supera limits.max_rps ({max})"
            ));
        }
        let concurrency = self.load.concurrency();
        if let Some(max) = limits.max_vus
            && concurrency > max
        {
            out.push(crate::tr!(
                "the concurrency ({concurrency}) exceeds limits.max_vus ({max})",
                "la concurrencia ({concurrency}) supera limits.max_vus ({max})"
            ));
        }
        if let Some(max) = limits.max_duration
            && self.load.duration() > max.0
        {
            out.push(crate::tr!(
                "the duration ({}) exceeds limits.max_duration ({max})",
                "la duración ({}) supera limits.max_duration ({max})",
                HumanDuration(self.load.duration())
            ));
        }
        out
    }

    /// Verdadero si todos los hosts son loopback (literal: `localhost` o una IP de loopback).
    pub fn loopback_only(&self) -> bool {
        self.hosts.iter().all(|h| is_loopback(h))
    }
}

/// Dónde se buscan las variables: iteración > `setup` > globales del plan.
pub struct Vars<'a> {
    pub plan: &'a Plan,
    /// Extraídas en `setup`.
    pub setup: &'a HashMap<String, Value>,
    /// Extraídas en esta iteración.
    pub locals: &'a HashMap<String, Value>,
}

impl Scope for Vars<'_> {
    fn var(&self, name: &str) -> Option<&Value> {
        self.locals
            .get(name)
            .or_else(|| self.setup.get(name))
            .or_else(|| self.plan.vars.get(name))
    }

    fn secret(&self, name: &str) -> Option<&str> {
        self.plan.secrets.get(name).map(String::as_str)
    }
}

/// Variables sin definir y secretos sin declarar o sin valor.
fn check_references(
    step: &StepPlan,
    known: &BTreeSet<String>,
    secrets: &BTreeMap<String, Option<String>>,
    columns: &BTreeMap<&str, &[String]>,
) -> Vec<String> {
    let mut problems = BTreeSet::new();
    for var in step.checks.iter().flat_map(|c| c.vars()) {
        if !known.contains(var) && var.contains('.') {
            problems.insert(crate::tr!(
                "a check uses {}",
                "un check usa {}",
                unknown_column(var, columns)
            ));
        } else if !known.contains(var) {
            problems.insert(crate::tr!(
                "a check uses `{{{{{var}}}}}`, which is not defined",
                "un check usa `{{{{{var}}}}}`, que no está definida"
            ));
        }
    }
    for t in step.templates() {
        for var in t.vars() {
            if !known.contains(var) && var.contains('.') {
                problems.insert(unknown_column(var, columns));
            } else if !known.contains(var) {
                problems.insert(crate::tr!("`{{{{{var}}}}}` is not defined: add it to `vars`, to the environment or with --var, or extract it in an earlier step", "`{{{{{var}}}}}` no está definida: agregala en `vars`, en el entorno o con --var, o extraéla en un paso anterior"
                ));
            }
        }
        for name in t.secrets() {
            match secrets.get(name) {
                None => problems.insert(crate::tr!("`{{{{env.{name}}}}}` is not declared in `secrets` of config.yaml", "`{{{{env.{name}}}}}` no está declarado en `secrets` de config.yaml"
                )),
                Some(None) => problems.insert(crate::tr!("secret `{name}` has no value: set it as an environment variable or in .env", "el secreto `{name}` no tiene valor: definilo como variable de entorno o en .env"
                )),
                Some(Some(_)) => false,
            };
        }
    }
    problems.into_iter().collect()
}

/// Arma las partes de un body multipart con los archivos ya leídos.
fn multipart_plan(
    spec: &indexmap::IndexMap<String, MultipartPart>,
    files: &BTreeMap<String, Upload>,
) -> Result<BodyPlan, String> {
    let mut parts = Vec::new();
    for (name, part) in spec {
        let plan = match part {
            MultipartPart::Text(t) => {
                PartPlan::Text(Template::parse(t).map_err(|e| format!("multipart.{name}: {e}"))?)
            }
            MultipartPart::File(f) => {
                let upload = files.get(&f.file).ok_or_else(|| {
                    crate::tr!(
                        "multipart.{name}: file {} was not loaded",
                        "multipart.{name}: el archivo {} no se cargó",
                        f.file
                    )
                })?;
                let default_name = std::path::Path::new(&f.file)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("archivo");
                PartPlan::File {
                    upload: upload.clone(),
                    filename: Template::parse(f.filename.as_deref().unwrap_or(default_name))
                        .map_err(|e| format!("multipart.{name}.filename: {e}"))?,
                    content_type: f
                        .content_type
                        .clone()
                        .unwrap_or_else(|| upload::content_type(&f.file).to_string()),
                }
            }
        };
        parts.push((name.clone(), plan));
    }
    // Un boundary que no aparezca en ningún archivo; los textos son cortos y lo hace improbable.
    let boundary = loop {
        let b = format!("lt-{:016x}{:016x}", fastrand::u64(..), fastrand::u64(..));
        let appears = parts.iter().any(|(_, p)| match p {
            PartPlan::File { upload, .. } => {
                upload.bytes.windows(b.len()).any(|w| w == b.as_bytes())
            }
            PartPlan::Text(_) => false,
        });
        if !appears {
            break b;
        }
    };
    Ok(BodyPlan::Multipart { boundary, parts })
}

/// Body `multipart/form-data` (RFC 7578). En los nombres, comillas y saltos de línea se
/// codifican como en los navegadores, para que no rompan el header de la parte.
fn multipart(
    boundary: &str,
    parts: &[(String, PartPlan)],
    scope: &dyn Scope,
) -> Result<Bytes, String> {
    let quote = |s: &str| {
        s.replace('"', "%22")
            .replace('\r', "%0D")
            .replace('\n', "%0A")
    };
    let size: usize = parts
        .iter()
        .map(|(_, p)| match p {
            PartPlan::File { upload, .. } => upload.bytes.len() + 200,
            PartPlan::Text(_) => 200,
        })
        .sum();
    let mut body = Vec::with_capacity(size + 64);
    for (name, part) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match part {
            PartPlan::Text(t) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                        quote(name)
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(t.render(scope)?.as_bytes());
            }
            PartPlan::File {
                upload,
                filename,
                content_type,
            } => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {content_type}\r\n\r\n",
                        quote(name),
                        quote(&filename.render(scope)?)
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(&upload.bytes);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok(Bytes::from(body))
}

/// Índices de los datasets que usan estos pasos.
fn datasets_used<'a>(
    data: &[DataPlan],
    steps: impl IntoIterator<Item = &'a StepPlan> + Clone,
) -> Vec<usize> {
    data.iter()
        .enumerate()
        .filter(|(_, d)| {
            steps
                .clone()
                .into_iter()
                .any(|s| s.uses_dataset(&d.dataset.name))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Por qué `dataset.columna` no se puede usar en este paso.
fn unknown_column(var: &str, columns: &BTreeMap<&str, &[String]>) -> String {
    let (dataset, column) = var.split_once('.').unwrap_or((var, ""));
    match columns.get(dataset) {
        None => crate::tr!(
            "`{{{{{var}}}}}`: there is no dataset `{dataset}` in `data`",
            "`{{{{{var}}}}}`: no hay un dataset `{dataset}` en `data`"
        ),
        Some(cols) if !cols.iter().any(|c| c == column) => crate::tr!(
            "`{{{{{var}}}}}`: dataset `{dataset}` has no column `{column}` (it has: {})",
            "`{{{{{var}}}}}`: el dataset `{dataset}` no tiene la columna `{column}` (tiene: {})",
            cols.join(", ")
        ),
        Some(_) => crate::tr!(
            "`{{{{{var}}}}}`: datasets cannot be used in `setup`, which runs only once; for a step per user (like the login), use `vu_setup`",
            "`{{{{{var}}}}}`: los datasets no se pueden usar en `setup`, que corre una sola vez; para un paso por usuario (como el login), usá `vu_setup`"
        ),
    }
}

pub fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn build_step(
    id: usize,
    flow: &str,
    step: &Step,
    defaults: Option<&Defaults>,
    base: &Url,
    allowed: &BTreeSet<String>,
    files: &BTreeMap<String, Upload>,
) -> Result<StepPlan, String> {
    let r = &step.request;
    let checks = step
        .checks
        .iter()
        .enumerate()
        .map(|(i, c)| Assertion::parse(c).map_err(|e| format!("checks[{i}]: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let extract = step
        .extract
        .iter()
        .map(|(name, spec)| {
            Extractor::parse(spec)
                .map(|e| (name.clone(), e))
                .map_err(|e| format!("extract.{name}: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (origin_url, path, raw_query) = split_url(base, &r.url)?;

    let https = match origin_url.scheme() {
        "http" => false,
        "https" => true,
        other => {
            return Err(crate::tr!(
                "scheme `{other}` is not supported",
                "esquema `{other}` no soportado"
            ));
        }
    };
    let host = origin_url
        .host_str()
        .ok_or(crate::tr!("the URL has no host", "la URL no tiene host"))?
        .to_ascii_lowercase();
    if !allowed.contains(&host) {
        return Err(crate::tr!(
            "host `{host}` is not in the allowed_hosts of the environment",
            "el host `{host}` no está en allowed_hosts del entorno"
        ));
    }
    let port = origin_url
        .port_or_known_default()
        .ok_or(crate::tr!("the URL has no port", "la URL no tiene puerto"))?;

    let mut query = Vec::new();
    if let Some(raw) = raw_query {
        query.push(QueryPart::Raw(Template::parse(&raw)?));
    }
    for (key, value) in &r.query {
        let template = match value {
            Value::String(s) => Template::parse(s)?,
            other => Template::literal(to_text(other)),
        };
        query.push(QueryPart::Pair(key.clone(), template));
    }

    let authority = match origin_url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    let mut headers: Vec<(HeaderName, Template)> = vec![
        (HOST, Template::literal(authority)),
        (
            USER_AGENT,
            Template::literal(concat!("crabfall/", env!("CARGO_PKG_VERSION"))),
        ),
        (ACCEPT_ENCODING, Template::literal(DEFAULT_ACCEPT_ENCODING)),
    ];
    let mut set_header = |name: &str, value: &str| -> Result<(), String> {
        let name = HeaderName::try_from(name).map_err(|e| format!("header `{name}`: {e}"))?;
        let value = Template::parse(value).map_err(|e| format!("header `{name}`: {e}"))?;
        headers.retain(|(n, _)| *n != name);
        headers.push((name, value));
        Ok(())
    };
    let body = match (&r.json, &r.body) {
        (Some(json), _) => {
            set_header(CONTENT_TYPE.as_str(), "application/json")?;
            BodyPlan::Json {
                template: ValueTemplate::parse(json)?,
                source: json.clone(),
            }
        }
        (None, Some(text)) => BodyPlan::Text(Template::parse(text)?),
        (None, None) if !r.form.is_empty() => {
            set_header(CONTENT_TYPE.as_str(), "application/x-www-form-urlencoded")?;
            let fields = r
                .form
                .iter()
                .map(|(name, value)| {
                    let template = match value {
                        Value::String(s) => Template::parse(s)?,
                        other => Template::literal(to_text(other)),
                    };
                    Ok((name.clone(), template))
                })
                .collect::<Result<Vec<_>, String>>()?;
            BodyPlan::Form(fields)
        }
        (None, None) if !r.multipart.is_empty() => multipart_plan(&r.multipart, files)?,
        (None, None) => BodyPlan::None,
    };
    for (name, value) in defaults.map(|d| &d.headers).into_iter().flatten() {
        set_header(name, value)?;
    }
    for (name, value) in &r.headers {
        set_header(name, value)?;
    }

    Ok(StepPlan {
        id,
        name: step.name.clone(),
        flow: flow.to_string(),
        tags: step.tags.clone(),
        source: step.source.clone(),
        method: Method::from_bytes(r.method.as_bytes()).map_err(|e| e.to_string())?,
        origin: Origin { https, host, port },
        path: Template::parse(&path)?,
        query,
        headers,
        body,
        extract,
        checks,
        timeout: r
            .timeout
            .or(defaults.and_then(|d| d.timeout))
            .map_or(DEFAULT_TIMEOUT, |t| t.0),
        redirects: r
            .redirects
            .or(defaults.and_then(|d| d.redirects))
            .unwrap_or(DEFAULT_REDIRECTS),
        think: step
            .think
            .or(defaults.and_then(|d| d.think))
            .filter(|t| !t.is_zero()),
    })
}

/// Separa una URL del escenario en (origen, path, query). `/products` se agrega al path de
/// `base_url` (`https://h/api` → `/api/products`); una URL absoluta define su propio origen,
/// que no puede tener variables.
fn split_url(base: &Url, url: &str) -> Result<(Url, String, Option<String>), String> {
    let (origin, prefix, rest) = match url.split_once("://") {
        Some((scheme, after)) => {
            let end = after.find(['/', '?']).unwrap_or(after.len());
            let authority = &after[..end];
            if authority.contains("{{") {
                return Err(crate::tr!(
                    "the URL host cannot have variables",
                    "el host de la URL no puede tener variables"
                ));
            }
            let origin = Url::parse(&format!("{scheme}://{authority}/"))
                .map_err(|e| crate::tr!("invalid url: {e}", "url inválida: {e}"))?;
            (origin, String::new(), &after[end..])
        }
        None => (
            base.clone(),
            base.path().trim_end_matches('/').to_string(),
            url,
        ),
    };
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q.to_string())),
        None => (rest, None),
    };
    let path = if path.is_empty() { "/" } else { path };
    Ok((origin, format!("{prefix}{path}"), query))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoVars;
    impl Scope for NoVars {
        fn var(&self, _: &str) -> Option<&Value> {
            None
        }
        fn secret(&self, _: &str) -> Option<&str> {
            None
        }
    }

    #[test]
    fn splits_relative_and_absolute_urls() {
        let base = Url::parse("https://api.example.com/v1/").unwrap();
        let (o, p, q) = split_url(&base, "/orders?x=1").unwrap();
        assert_eq!(
            (o.host_str(), p.as_str(), q.as_deref()),
            (Some("api.example.com"), "/v1/orders", Some("x=1"))
        );

        let (o, p, q) = split_url(&base, "http://localhost:3000/orders/{{id}}").unwrap();
        assert_eq!(
            (o.port(), p.as_str(), q),
            (Some(3000), "/orders/{{id}}", None)
        );

        assert!(split_url(&base, "https://{{host}}/x").is_err());
    }

    #[test]
    fn encodes_interpolated_values_only() {
        let step = StepPlan {
            id: 0,
            name: "s".into(),
            flow: "f".into(),
            tags: vec![],
            source: None,
            method: Method::GET,
            origin: Origin {
                https: false,
                host: "h".into(),
                port: 80,
            },
            path: Template::parse("/a%20b/{{$randomInt(1,1)}}").unwrap(),
            query: vec![QueryPart::Pair("q".into(), Template::literal("x y&z"))],
            headers: vec![],
            body: BodyPlan::None,
            extract: vec![],
            checks: vec![],
            timeout: DEFAULT_TIMEOUT,
            redirects: DEFAULT_REDIRECTS,
            think: None,
        };
        assert_eq!(step.target(&NoVars).unwrap(), "/a%20b/1?q=x+y%26z");
    }

    #[test]
    fn detects_loopback_hosts() {
        assert!(is_loopback("localhost"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("[::1]"));
        assert!(!is_loopback("api.example.com"));
        assert!(!is_loopback("10.0.0.1"));
    }
}
