//! La guarda vista desde el binario (ADR-004, sección Verificación). Estos tests no
//! tocan el keyring: todos los casos se rechazan antes de necesitar la clave.

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

const REFUSED: i32 = 3;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lt-guard-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".loadtest/scenarios")).unwrap();
    std::fs::write(
        dir.join(".loadtest/config.yaml"),
        "version: 1\nenvironments:\n  local:\n    base_url: http://127.0.0.1:9\n    allowed_hosts: [127.0.0.1]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".loadtest/scenarios/s.yaml"),
        "version: 1\nname: s\nflows:\n  main:\n    steps:\n      - name: a\n        request: { method: GET, url: /x }\n\
         load:\n  executor: constant_rate\n  rate: 1/s\n  duration: 1s\n  max_vus: 1\n",
    )
    .unwrap();
    dir
}

fn lt(dir: &PathBuf, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lt"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn approve_with_piped_stdin_is_refused() {
    let dir = project("pipe");
    let out = lt(&dir, &["approve", "s"], "127.0.0.1\n");
    assert_eq!(out.status.code(), Some(REFUSED), "{}", stderr(&out));
    assert!(stderr(&out).contains("terminal interactiva"));
    assert!(!dir.join(".loadtest/approvals").exists());
}

#[test]
fn run_without_approval_is_refused() {
    let dir = project("missing");
    let out = lt(&dir, &["run", "s"], "");
    assert_eq!(out.status.code(), Some(REFUSED), "{}", stderr(&out));
    assert!(stderr(&out).contains("no está aprobado"));
    assert!(!dir.join(".loadtest/runs").exists());
}

#[test]
fn run_with_a_broken_approval_file_is_refused() {
    let dir = project("broken");
    std::fs::create_dir_all(dir.join(".loadtest/approvals")).unwrap();
    std::fs::write(
        dir.join(".loadtest/approvals/s.local.json"),
        "{ \"hash\": \"x\" }",
    )
    .unwrap();
    let out = lt(&dir, &["run", "s"], "");
    assert_eq!(out.status.code(), Some(REFUSED), "{}", stderr(&out));
    assert!(stderr(&out).contains("dañado"));
}
