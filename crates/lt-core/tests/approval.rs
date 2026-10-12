//! Tests de la guarda de aprobación (ADR-004, sección Verificación).

use lt_core::{
    approval::{self, Approval, MemoryKey, Rejection},
    config::Environment,
    engine::plan::Plan,
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
    Plan::build(&parse_scenario(scenario).unwrap(), env).unwrap()
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
