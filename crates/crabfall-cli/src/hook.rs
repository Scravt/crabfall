//! `crabfall hook pre-tool-use`: hook `PreToolUse` de Claude Code (ADR-001, ADR-004 §5).
//!
//! Lee el evento por stdin y bloquea (código 2) los comandos de shell que ejecutarían
//! `crabfall run` (salvo con `--ask`, ADR-017) o `crabfall approve`. Es una defensa extra: la
//! guarda real está en el binario.

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
        Some("tui") => {
            eprintln!(
                "{}",
                crabfall_core::tr!(
                    "crabfall without a subcommand opens the interactive TUI, which approves and runs load: only a person uses it (ADR-004, ADR-015).                  You can use `crabfall validate` and `crabfall check`.",
                    "crabfall sin subcomando abre la TUI interactiva, que aprueba y corre carga: la usa solo una persona (ADR-004, ADR-015).                  Vos podés usar `crabfall validate` y `crabfall check`."
                )
            );
            2
        }
        Some("run") => {
            eprintln!(
                "{}",
                crabfall_core::tr!(
                    "crabfall run generates load, and a person has to confirm it (ADR-004, ADR-017). \
                 Use `crabfall run ... --ask`: it opens a window where the person confirms, and the load runs there. \
                 Run it in the background and wait: the person may take a few minutes.",
                    "crabfall run genera carga, y una persona tiene que confirmarla (ADR-004, ADR-017). \
                 Usá `crabfall run ... --ask`: abre una ventana donde la persona confirma, y la carga corre ahí. \
                 Correlo en segundo plano y esperá: la persona puede tardar unos minutos."
                )
            );
            2
        }
        Some(sub) => {
            eprintln!(
                "{}",
                crabfall_core::tr!(
                    "crabfall {sub} generates load or authorizes it, and only a person runs it (ADR-004). \
                 Ask the person to run `crabfall {sub} ...` in their terminal. \
                 You can use `crabfall validate` and `crabfall check`.",
                    "crabfall {sub} genera carga o la autoriza, y lo ejecuta solo una persona (ADR-004). \
                 Pedile a la persona que corra `crabfall {sub} ...` en su terminal. \
                 Vos podés usar `crabfall validate` y `crabfall check`."
                )
            );
            2
        }
        None => 0,
    }
}

/// Devuelve el subcomando bloqueado si `command` invoca `crabfall run`/`crabfall approve`,
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
                .position(|w| is_crabfall(w))
                .map(|i| i + 1)
                .or_else(|| {
                    // `cargo run -p crabfall-cli -- run x` o `cargo run --bin crabfall -- run x`
                    let cargo_crabfall = words.first() == Some(&"cargo")
                        && words.windows(2).any(|w| {
                            matches!(
                                w,
                                ["-p" | "--package", "crabfall-cli"] | ["--bin", "crabfall"]
                            )
                        });
                    cargo_crabfall.then(|| {
                        words
                            .iter()
                            .position(|w| *w == "--")
                            .map_or(words.len(), |i| i + 1)
                    })
                })?;
            let mut rest = words[start..].iter();
            while let Some(w) = rest.next() {
                match *w {
                    // La ayuda y la versión no hacen nada.
                    "--help" | "-h" | "--version" | "-V" => return None,
                    "--lang" => {
                        rest.next();
                    }
                    w if w.starts_with('-') => {}
                    // `run --ask` pide la confirmación en una ventana aparte (ADR-017).
                    // `--window` es la de esa ventana: no lo lanza nadie más.
                    "run" => {
                        let flags: Vec<&str> = rest.copied().collect();
                        let window = flags.iter().any(|f| f.starts_with("--window"));
                        let ask = flags.contains(&"--ask");
                        return (window || !ask).then_some("run");
                    }
                    sub => return HUMAN_ONLY.iter().find(|h| **h == sub).copied(),
                }
            }
            // Sin subcomando, `crabfall` abre la TUI (ADR-015), que también aprueba y corre.
            // Solo si es el comando del segmento: `which crabfall` no la abre.
            let invoked = words
                .iter()
                .take_while(|w| w.contains('=') && !w.starts_with('-'))
                .count()
                + 1
                == start;
            invoked.then_some("tui")
        })
}

fn is_crabfall(word: &str) -> bool {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    name == "crabfall"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_run_and_approve() {
        assert_eq!(blocked_subcommand("crabfall run checkout"), Some("run"));
        assert_eq!(
            blocked_subcommand("crabfall approve checkout --env staging"),
            Some("approve")
        );
        assert_eq!(
            blocked_subcommand("cd api && ./target/release/crabfall.exe run x"),
            Some("run")
        );
        assert_eq!(
            blocked_subcommand(r#""C:\tools\crabfall.exe" run x"#),
            Some("run")
        );
        assert_eq!(
            blocked_subcommand("crabfall validate x; crabfall run x"),
            Some("run")
        );
        assert_eq!(
            blocked_subcommand("cargo run -p crabfall-cli -- run x"),
            Some("run")
        );
        assert_eq!(
            blocked_subcommand("cargo run --release --bin crabfall -- approve x"),
            Some("approve")
        );
    }

    #[test]
    fn allows_run_only_with_ask() {
        assert_eq!(blocked_subcommand("crabfall run checkout --ask"), None);
        assert_eq!(
            blocked_subcommand("crabfall run checkout --env local --rate 20/s --ask --json"),
            None
        );
        assert_eq!(
            blocked_subcommand("crabfall run x --ask --window C:/t.json"),
            Some("run")
        );
        assert_eq!(
            blocked_subcommand("crabfall run x --window=t.json"),
            Some("run")
        );
        assert_eq!(
            blocked_subcommand("crabfall approve x --ask"),
            Some("approve")
        );
        assert_eq!(
            blocked_subcommand("crabfall run x --ask; crabfall run x"),
            Some("run")
        );
    }

    #[test]
    fn allows_everything_else() {
        assert_eq!(
            blocked_subcommand("crabfall validate checkout --json"),
            None
        );
        assert_eq!(blocked_subcommand("crabfall check checkout"), None);
        assert_eq!(blocked_subcommand("crabfall schema"), None);
        assert_eq!(blocked_subcommand("npm run dev"), None);
        assert_eq!(blocked_subcommand("cargo run -p lab-server"), None);
        assert_eq!(blocked_subcommand("cat crabfall-run.txt"), None);
        assert_eq!(blocked_subcommand("crabfall --help"), None);
        assert_eq!(blocked_subcommand("crabfall --version"), None);
        assert_eq!(blocked_subcommand("which crabfall"), None);
        assert_eq!(blocked_subcommand("crabfall --lang es validate x"), None);
    }

    #[test]
    fn blocks_the_tui() {
        assert_eq!(blocked_subcommand("crabfall"), Some("tui"));
        assert_eq!(blocked_subcommand("crabfall --lang es"), Some("tui"));
        assert_eq!(blocked_subcommand("CRABFALL_LANG=es crabfall"), Some("tui"));
        assert_eq!(blocked_subcommand("cd api && crabfall"), Some("tui"));
        assert_eq!(blocked_subcommand("crabfall --lang es run x"), Some("run"));
    }
}
