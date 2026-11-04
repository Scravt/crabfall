//! Tests de la guarda de aprobación (ADR-004, sección Verificación).

use crabfall_core::{
    approval::{self, Approval, MemoryKey, Rejection},
    config::Environment,
    engine::plan::{DEFAULT_USER_AGENT, Inputs, Plan},
    validate::parse_scenario,
};

const SCENARIO: &str = "version: 1
name: checkout
description: original
flows:
  main:
    steps:
      - name: list
        request: { method: GET, url: /products }
        source: { file: src/a.ts }
load:
  executor: constant_rate
  rate: 10/s
  duration: 10s
  max_vus: 10
";

const ENV: &str = "base_url: http://127.0.0.1:3000
allowed_hosts: [127.0.0.1]
limits: { max_rps: 100 }
";

fn plan(scenario: &str, env: &Environment) -> Plan {
    Plan::build(&parse_scenario(scenario).unwrap(), env, &Default::default()).unwrap()
}

fn env(yaml: &str) -> Environment {
    serde_saphyr::from_str(yaml).unwrap()
}

fn key() -> MemoryKey {
    MemoryKey(b"clave-de-prueba".to_vec())
}

fn approved() -> Approval {
    let e = env(ENV);
    approval::approve(
        &plan(SCENARIO, &e),
        &e,
        "local",
        "gonzalo",
        "2026-10-12T00:00:00Z",
        &key(),
    )
    .unwrap()
}

fn verify(a: Option<&Approval>, scenario: &str, env_yaml: &str) -> Result<(), Rejection> {
    let e = env(env_yaml);
    approval::verify(a, &plan(scenario, &e), &e, "local", &key())
}

#[test]
fn approved_plan_passes() {
    assert_eq!(verify(Some(&approved()), SCENARIO, ENV), Ok(()));
}

#[test]
fn missing_approval_is_rejected() {
    assert_eq!(verify(None, SCENARIO, ENV), Err(Rejection::Missing));
}

#[test]
fn description_meta_and_source_do_not_invalidate() {
    let edited = SCENARIO
        .replace(
            "description: original",
            "description: otra\nmeta: { created_by: agent }",
        )
        .replace("src/a.ts", "src/b.ts");
    assert_eq!(verify(Some(&approved()), &edited, ENV), Ok(()));
}

#[test]
fn editing_the_scenario_invalidates_and_shows_the_diff() {
    let edited = SCENARIO.replace("rate: 10/s", "rate: 90/s");
    let Err(Rejection::Changed(diff)) = verify(Some(&approved()), &edited, ENV) else {
        panic!("tenía que rechazar");
    };
    assert!(diff.contains("-    \"rate\": \"10/s\""), "{diff}");
    assert!(diff.contains("+    \"rate\": \"90/s\""), "{diff}");
}

#[test]
fn changing_the_environment_invalidates() {
    let changes = [
        ENV.replace("http://127.0.0.1:3000", "http://127.0.0.1:4000"),
        ENV.replace("[127.0.0.1]", "[127.0.0.1, localhost]"),
        ENV.replace("max_rps: 100", "max_rps: 1000"),
        format!("{ENV}vars: {{ product_id: 7 }}\n"),
    ];
    for changed in changes {
        assert!(
            matches!(
                verify(Some(&approved()), SCENARIO, &changed),
                Err(Rejection::Changed(_))
            ),
            "{changed}"
        );
    }
}

#[test]
fn edited_approval_file_is_rejected() {
    // Recalcular el hash para el plan nuevo no alcanza: la firma no coincide.
    let e = env(ENV);
    let edited_plan = plan(&SCENARIO.replace("rate: 10/s", "rate: 90/s"), &e);
    let mut forged = approved();
    forged.plan = edited_plan.canonical(&e);
    forged.hash = approval::fingerprint(&forged.plan);
    assert!(matches!(
        approval::verify(Some(&forged), &edited_plan, &e, "local", &key()),
        Err(Rejection::Tampered(_))
    ));

    let mut renamed = approved();
    renamed.approved_by = "otra persona".into();
    assert!(matches!(
        verify(Some(&renamed), SCENARIO, ENV),
        Err(Rejection::Tampered(_))
    ));
}

#[test]
fn approval_copied_from_another_scenario_or_environment_is_rejected() {
    let other = SCENARIO.replace("name: checkout", "name: otro");
    assert!(matches!(
        verify(Some(&approved()), &other, ENV),
        Err(Rejection::Tampered(_))
    ));

    let e = env(ENV);
    let result = approval::verify(
        Some(&approved()),
        &plan(SCENARIO, &e),
        &e,
        "staging",
        &key(),
    );
    assert!(matches!(result, Err(Rejection::Tampered(_))));
}

#[test]
fn approval_signed_with_another_key_is_rejected() {
    let e = env(ENV);
    let other_key = MemoryKey(b"otra-clave".to_vec());
    let result = approval::verify(
        Some(&approved()),
        &plan(SCENARIO, &e),
        &e,
        "local",
        &other_key,
    );
    assert!(matches!(result, Err(Rejection::Tampered(_))));
}

#[test]
fn an_allowance_lets_the_same_plan_run_until_it_expires() {
    let a = approved();
    let allowance = approval::allow(&a, 1_000, &key()).unwrap();
    assert!(approval::allows(&allowance, &a, 999, &key()));
    assert!(!approval::allows(&allowance, &a, 1_000, &key()));
}

#[test]
fn an_allowance_does_not_survive_a_new_plan_or_an_edit() {
    let a = approved();
    let allowance = approval::allow(&a, 1_000, &key()).unwrap();

    let e = env(ENV);
    let changed = SCENARIO.replace("rate: 10/s", "rate: 20/s");
    let other = approval::approve(
        &plan(&changed, &e),
        &e,
        "local",
        "gonzalo",
        "2026-10-12T00:00:00Z",
        &key(),
    )
    .unwrap();
    assert!(!approval::allows(&allowance, &other, 0, &key()));

    let mut extended = allowance.clone();
    extended.until = 9_999;
    assert!(!approval::allows(&extended, &a, 2_000, &key()));

    let other_key = MemoryKey(b"otra-clave".to_vec());
    assert!(!approval::allows(&allowance, &a, 0, &other_key));
}

#[test]
fn a_var_by_flag_invalidates_but_a_secret_value_does_not() {
    const WITH_VARS: &str = "version: 1
name: checkout
vars: { id: 7 }
flows:
  main:
    steps:
      - name: item
        request:
          method: GET
          url: /products/{{id}}
          headers: { Authorization: \"Bearer {{env.TOKEN}}\" }
load:
  executor: constant_rate
  rate: 10/s
  duration: 10s
  max_vus: 10
";
    let e = env(ENV);
    let build = |vars: &[(&str, serde_json::Value)], token: &str| {
        let inputs = Inputs {
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            secrets: [("TOKEN".to_string(), Some(token.to_string()))].into(),
            ..Default::default()
        };
        Plan::build(&parse_scenario(WITH_VARS).unwrap(), &e, &inputs).unwrap()
    };
    let a = approval::approve(
        &build(&[], "uno"),
        &e,
        "local",
        "gonzalo",
        "2026-10-12T00:00:00Z",
        &key(),
    )
    .unwrap();
    let verify = |plan: &Plan| approval::verify(Some(&a), plan, &e, "local", &key());

    // Otro valor del secreto no cambia la firma: no entra en el plan canónico.
    assert_eq!(verify(&build(&[], "dos")), Ok(()));
    // `--var id=7` repite el valor del escenario: el plan es el mismo.
    assert_eq!(verify(&build(&[("id", 7.into())], "uno")), Ok(()));
    // `--var id=8` cambia el tráfico: hay que aprobar de nuevo, con el diff.
    let Err(Rejection::Changed(diff)) = verify(&build(&[("id", 8.into())], "uno")) else {
        panic!("tenía que rechazar");
    };
    assert!(
        diff.contains("-      7") && diff.contains("+      8"),
        "{diff}"
    );
}

#[test]
fn the_default_user_agent_is_signed_without_the_version() {
    let e = env(ENV);
    let canonical = plan(SCENARIO, &e).canonical(&e).to_string();
    assert!(canonical.contains("crabfall/<version>"), "{canonical}");
    assert!(!canonical.contains(DEFAULT_USER_AGENT), "{canonical}");

    // Uno puesto en el escenario se firma tal cual.
    let custom = SCENARIO.replace(
        "url: /products }",
        "url: /products, headers: { User-Agent: mi-cliente/1 } }",
    );
    let canonical = plan(&custom, &e).canonical(&e).to_string();
    assert!(canonical.contains("mi-cliente/1"), "{canonical}");
    assert!(!canonical.contains("crabfall/<version>"), "{canonical}");
}

#[test]
fn diagnosis_settings_are_not_signed_and_the_scenario_adds_to_the_environment() {
    let e = env(&format!(
        "{ENV}diagnosis: {{ off: [long_tail], latency_margin: 80ms, min_requests: 20 }}\n"
    ));
    let tuned = format!("{SCENARIO}diagnosis:\n  off: [time_split]\n  min_requests: 50\n");
    let p = plan(&tuned, &e);
    assert_eq!(p.diagnosis.off, ["long_tail", "time_split"]);
    assert_eq!(p.diagnosis.latency_margin_ms, Some(80.0));
    assert_eq!(p.diagnosis.min_requests, Some(50));
    // No cambian el tráfico: la aprobación de siempre sigue valiendo.
    assert_eq!(
        approval::verify(Some(&approved()), &p, &e, "local", &key()),
        Ok(())
    );
}
