//! `lt hook pre-tool-use`: hook `PreToolUse` de Claude Code (ADR-001, ADR-004 §5).
//!
//! Lee el evento por stdin y bloquea (código 2) los comandos de shell que ejecutarían
//! `lt run` o `lt approve`. Es una defensa extra: la guarda real está en el binario.

use std::io::Read;

use serde_json::Value;

/// Subcomandos que solo puede ejecutar una persona.
const HUMAN_ONLY: &[&str] = &["run", "approve"];

pub fn cmd_pre_tool_use() -> u8 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(event) = serde_json::from_str::<Value>(&input) else {
        return 0;
    };
    if event["tool_name"] != "Bash" {
        return 0;
    }
    let command = event["tool_input"]["command"].as_str().unwrap_or_default();
    match blocked_subcommand(command) {
        Some(sub) => {
            eprintln!(
                "lt {sub} genera carga o la autoriza, y lo ejecuta solo una persona (ADR-004). \
                 Pedile a la persona que corra `lt {sub} ...` en su terminal. \
                 Vos podés usar `lt validate` y `lt check`."
            );
            2
        }
        None => 0,
    }
}

/// Devuelve el subcomando bloqueado si `command` invoca `lt run`/`lt approve`,
/// directamente o vía `cargo run`.
pub fn blocked_subcommand(command: &str) -> Option<&'static str> {
    command
        .split(['\n', ';', '|', '&', '(', ')', '`'])
        .find_map(|segment| {
            let words: Vec<&str> = segment
                .split_whitespace()
                .map(|w| w.trim_matches(['"', '\'']))
                .collect();
            let start = words
                .iter()
                .position(|w| is_lt(w))
                .map(|i| i + 1)
                .or_else(|| {
                    // `cargo run -p lt-cli -- run x` o `cargo run --bin lt -- run x`
                    let cargo_lt = words.first() == Some(&"cargo")
                        && words
                            .windows(2)
                            .any(|w| matches!(w, ["-p" | "--package", "lt-cli"] | ["--bin", "lt"]));
                    cargo_lt.then(|| {
                        words
                            .iter()
                            .position(|w| *w == "--")
                            .map_or(words.len(), |i| i + 1)
                    })
                })?;
            let sub = words[start..].iter().find(|w| !w.starts_with('-'))?;
            HUMAN_ONLY.iter().find(|h| *h == sub).copied()
        })
}

fn is_lt(word: &str) -> bool {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    name == "lt"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_run_and_approve() {
        assert_eq!(blocked_subcommand("lt run checkout"), Some("run"));
        assert_eq!(
            blocked_subcommand("lt approve checkout --env staging"),
            Some("approve")
        );
        assert_eq!(
            blocked_subcommand("cd api && ./target/release/lt.exe run x"),
            Some("run")
        );
        assert_eq!(
            blocked_subcommand(r#""C:\tools\lt.exe" run x"#),
            Some("run")
        );
        assert_eq!(blocked_subcommand("lt validate x; lt run x"), Some("run"));
        assert_eq!(
            blocked_subcommand("cargo run -p lt-cli -- run x"),
            Some("run")
        );
        assert_eq!(
            blocked_subcommand("cargo run --release --bin lt -- approve x"),
            Some("approve")
        );
    }

    #[test]
    fn allows_everything_else() {
        assert_eq!(blocked_subcommand("lt validate checkout --json"), None);
        assert_eq!(blocked_subcommand("lt check checkout"), None);
        assert_eq!(blocked_subcommand("lt schema"), None);
        assert_eq!(blocked_subcommand("npm run dev"), None);
        assert_eq!(blocked_subcommand("cargo run -p lab-server"), None);
        assert_eq!(blocked_subcommand("cat lt-run.txt"), None);
    }
}
