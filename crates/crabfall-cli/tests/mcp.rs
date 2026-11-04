//! `crabfall mcp` visto desde un cliente (ADR-019): el protocolo por stdio y que cada
//! herramienta diga lo mismo que la CLI. `request_run` solo se prueba en lo que rechaza antes
//! de abrir la ventana (un escenario inválido o fuera de los límites): no abre ninguna.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

/// Un servidor HTTP mínimo en loopback que responde 200 a todo, para `check`.
fn ok_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
        }
    });
    port
}

fn project(port: u16) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crabfall-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".crabfall/scenarios")).unwrap();
    std::fs::write(
        dir.join(".crabfall/config.yaml"),
        format!(
            "version: 1\nenvironments:\n  local:\n    base_url: http://127.0.0.1:{port}\n    allowed_hosts: [127.0.0.1]\n    limits: {{ max_vus: 20 }}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join(".crabfall/scenarios/smoke.yaml"),
        "version: 1\nname: smoke\ndescription: A quick look.\nflows:\n  main:\n    steps:\n      - name: home\n        request: { method: GET, url: /ok }\n        checks: [{ status: 200 }]\n\
         load:\n  executor: constant_rate\n  rate: 1/s\n  duration: 1s\n  max_vus: 1\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".crabfall/scenarios/broken.yaml"),
        "version: 1\nname: broken\n",
    )
    .unwrap();
    dir
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    fn start(dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_crabfall"))
            .args(["mcp", "--project"])
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next: 0,
        }
    }

    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )["result"]
            .clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn an_mcp_client_sees_the_same_as_the_cli() {
    let dir = project(ok_server());
    let mut c = Client::start(&dir);

    let init = c.request(
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "crabfall");
    // Una notificación no lleva respuesta: la próxima línea es la de `tools/list`.
    c.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    let tools = c.request("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "list_scenarios",
            "schema",
            "validate",
            "check",
            "request_run",
            "findings",
            "compare"
        ]
    );
    // No hay forma de aprobar ni de correr sin una persona.
    assert!(!names.iter().any(|n| n.contains("approve")));

    let list = c.call("list_scenarios", json!({}));
    let scenarios = &list["structuredContent"]["scenarios"];
    assert_eq!(scenarios[0]["name"], "broken");
    assert!(scenarios[0]["invalid"].is_string(), "{list}");
    assert_eq!(scenarios[1]["description"], "A quick look.");
    assert_eq!(list["structuredContent"]["environments"], json!(["local"]));

    let valid = c.call("validate", json!({ "scenario": "smoke" }));
    assert_eq!(valid["structuredContent"]["exit_code"], 0, "{valid}");
    assert_eq!(valid["structuredContent"]["output"]["valid"], true);

    // Un ajuste que no aplica: la ruta y el flag, como en la CLI.
    let tweaked = c.call(
        "validate",
        json!({ "scenario": "smoke", "tweaks": { "vus": 5 } }),
    );
    assert_eq!(tweaked["structuredContent"]["exit_code"], 2, "{tweaked}");
    let errors = tweaked["structuredContent"]["output"]["errors"].to_string();
    assert!(errors.contains("--vus"), "{errors}");

    // Flags internos no pasan.
    let sneaky = c.call(
        "validate",
        json!({ "scenario": "smoke", "tweaks": { "window": "x.json" } }),
    );
    assert_eq!(sneaky["isError"], true, "{sneaky}");

    let check = c.call("check", json!({ "scenario": "smoke" }));
    assert_eq!(check["structuredContent"]["exit_code"], 0, "{check}");
    assert_eq!(check["structuredContent"]["output"]["ok"], true);

    // `request_run` rechaza antes de abrir la ventana: un escenario inválido…
    let broken = c.call("request_run", json!({ "scenario": "broken" }));
    assert_eq!(broken["structuredContent"]["exit_code"], 2, "{broken}");
    // …o uno que supera los límites del entorno (max_vus 20).
    let over = c.call(
        "request_run",
        json!({ "scenario": "smoke", "tweaks": { "max_vus": 50 } }),
    );
    assert_eq!(over["structuredContent"]["exit_code"], 3, "{over}");
    assert!(
        over["structuredContent"]["messages"]
            .as_str()
            .unwrap()
            .contains("max_vus"),
        "{over}"
    );

    // Sin corridas, `findings` dice por qué, como la CLI.
    let findings = c.call("findings", json!({}));
    assert_eq!(findings["structuredContent"]["exit_code"], 2, "{findings}");

    let unknown = c.request("tools/call", json!({ "name": "approve", "arguments": {} }));
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    let ping = c.request("ping", json!({}));
    assert_eq!(ping["result"], json!({}));
}
