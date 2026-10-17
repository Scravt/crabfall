//! Ajustes por flag (ADR-008) vistos desde el binario, con `lt validate`.

use std::{
    path::PathBuf,
    process::{Command, Output},
};

const INVALID: i32 = 2;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lt-tweaks-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".loadtest/scenarios")).unwrap();
    std::fs::create_dir_all(dir.join(".loadtest/data")).unwrap();
    std::fs::write(
        dir.join(".loadtest/config.yaml"),
        "version: 1\nenvironments:\n  local:\n    base_url: http://127.0.0.1:9\n    allowed_hosts: [127.0.0.1]\n",
    )
    .unwrap();
    std::fs::write(dir.join(".loadtest/data/users.csv"), "id\n1\n").unwrap();
    std::fs::write(
        dir.join(".loadtest/scenarios/s.yaml"),
        "version: 1\nname: s\ndata:\n  users: { file: data/users.csv }\nflows:\n  main:\n    steps:\n      - name: a\n        request: { method: GET, url: \"/x/{{users.id}}\" }\n\
         load:\n  executor: constant_rate\n  rate: 1/s\n  duration: 1s\n  max_vus: 1\n",
    )
    .unwrap();
    dir
}

fn lt(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lt"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn valid_tweaks_pass_validate() {
    let dir = project("ok");
    let o = lt(
        &dir,
        &[
            "validate",
            "s",
            "--rate",
            "100/s",
            "--duration",
            "2m",
            "--think",
            "1s-3s",
            "--pick",
            "users=random",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
}

#[test]
fn tweaks_that_do_not_fit_say_which_flag_is_missing() {
    let dir = project("bad");
    let o = lt(
        &dir,
        &["validate", "s", "--executor", "constant_vus", "--json"],
    );
    assert_eq!(o.status.code(), Some(INVALID), "{}", out(&o));
    assert!(out(&o).contains("(usá --vus)"), "{}", out(&o));

    let o = lt(
        &dir,
        &["validate", "s", "--data", "users=data/no-existe.csv"],
    );
    assert_eq!(o.status.code(), Some(INVALID));
    assert!(out(&o).contains("no-existe.csv"), "{}", out(&o));

    let o = lt(&dir, &["validate", "s", "--pick", "otros=random"]);
    assert!(
        out(&o).contains("no hay un dataset `otros` (hay: users)"),
        "{}",
        out(&o)
    );

    // Valores mal escritos los rechaza clap, con el mensaje de la unidad.
    let o = lt(&dir, &["validate", "s", "--think", "3s-1s"]);
    assert_eq!(o.status.code(), Some(INVALID));
    assert!(out(&o).contains("mayor que el máximo"), "{}", out(&o));
}

#[test]
fn thresholds_by_flag_are_checked_like_the_yaml() {
    let dir = project("thresholds");
    let o = lt(
        &dir,
        &[
            "validate",
            "s",
            "--threshold",
            "http.latency.p95<300ms",
            "--threshold",
            "http.rps>=1",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));

    let o = lt(
        &dir,
        &[
            "validate",
            "s",
            "--threshold",
            "http.latency.p95{step=nope}<1s",
        ],
    );
    assert_eq!(o.status.code(), Some(INVALID));
    assert!(out(&o).contains("ningún paso"), "{}", out(&o));

    let o = lt(&dir, &["validate", "s", "--threshold", "http.error_rate<1"]);
    assert_eq!(o.status.code(), Some(INVALID));
    assert!(out(&o).contains("porcentaje"), "{}", out(&o));
}
