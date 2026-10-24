//! La ventana de `--ask` en Linux (ADR-017): una terminal nueva en el escritorio.
//!
//! Algunas terminales vuelven enseguida (la ventana la abre un servidor, como en
//! gnome-terminal), así que `delegate` no espera al proceso que lanza: espera la respuesta, o
//! que el proceso de la ventana (su pid queda al lado de la respuesta) deje de existir.

use std::{
    ffi::{OsStr, OsString},
    io::IsTerminal,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use crabfall_core::{i18n::pick, tr};

use super::pid_path;

/// Cuánto puede tardar la terminal en arrancar la ventana.
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// Las terminales que se prueban, en orden, después de `$TERMINAL`.
const TERMINALS: &[&str] = &[
    "x-terminal-emulator",
    "gnome-terminal",
    "ptyxis",
    "konsole",
    "xfce4-terminal",
    "mate-terminal",
    "kitty",
    "alacritty",
    "wezterm",
    "foot",
    "xterm",
];

/// Los procesos que pueden ser padre de la ventana: las terminales (o su servidor).
const KNOWN_PARENTS: &[&str] = &[
    "xterm",
    "gnome-terminal-server",
    "ptyxis-agent",
    "konsole",
    "xfce4-terminal",
    "mate-terminal",
    "kitty",
    "alacritty",
    "wezterm-gui",
    "foot",
    "lxterminal",
    "qterminal",
    "tilix",
    "terminology",
    "urxvt",
    "sakura",
    "kgx",
];

/// Lo que no es una terminal en una ventana aunque `$TERMINAL` lo diga: shells y programas
/// que le dan una terminal a otro proceso sin mostrarla.
const NOT_A_WINDOW: &[&str] = &[
    "script", "tmux", "screen", "expect", "unbuffer", "socat", "ssh", "sshd", "sh", "bash", "dash",
    "zsh", "fish", "env", "setsid", "nohup", "sudo", "crabfall",
];

pub fn open_window(args: &[OsString], answer: &Path) -> std::io::Result<()> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err(std::io::Error::other(pick(
            "there is no desktop to open it on (neither DISPLAY nor WAYLAND_DISPLAY); \
             ask the person to run it in their terminal",
            "no hay escritorio donde abrirla (ni DISPLAY ni WAYLAND_DISPLAY); \
             pedile a la persona que lo corra en su terminal",
        )));
    }
    let terminal = find_terminal().ok_or_else(|| {
        std::io::Error::other(pick(
            "no terminal found; install one or set TERMINAL",
            "no se encontró una terminal; instalá una o definí TERMINAL",
        ))
    })?;
    let name = file_name(&terminal);
    let exe = std::env::current_exe()?;
    let cwd = std::env::current_dir()?;
    let mut launcher = Command::new(&terminal)
        .args(launch_args(&name, &cwd, &exe, args))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let started = Instant::now();
    let mut launcher_running = true;
    loop {
        if answer.exists() {
            return Ok(());
        }
        let window = std::fs::read_to_string(pid_path(answer))
            .ok()
            .and_then(|p| p.trim().parse::<u32>().ok());
        match window {
            // Se cerró sin responder: `delegate` lo cuenta.
            Some(pid) if !alive(pid) => return Ok(()),
            Some(_) => {}
            None if started.elapsed() > START_TIMEOUT => {
                let _ = launcher.kill();
                return Err(std::io::Error::other(tr!(
                    "`{name}` did not open the window in {} s",
                    "`{name}` no abrió la ventana en {} s",
                    START_TIMEOUT.as_secs()
                )));
            }
            None => {}
        }
        if launcher_running && let Some(status) = launcher.try_wait()? {
            launcher_running = false;
            if !status.success() && window.is_none() && !answer.exists() {
                return Err(std::io::Error::other(tr!(
                    "`{name}` failed ({status})",
                    "`{name}` falló ({status})"
                )));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `$TERMINAL` (solo el programa, sin sus argumentos) o la primera de `TERMINALS` en el PATH.
fn find_terminal() -> Option<PathBuf> {
    let preferred = std::env::var("TERMINAL").ok();
    let preferred = preferred
        .as_deref()
        .and_then(|t| t.split_whitespace().next());
    preferred
        .into_iter()
        .chain(TERMINALS.iter().copied())
        .find_map(in_path)
}

fn in_path(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        return Path::new(program).is_file().then(|| program.into());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

/// Cómo le pide cada terminal que corra `exe args` en una ventana nueva, y que espere a que
/// termine cuando sabe hacerlo.
fn launch_args(terminal: &str, cwd: &Path, exe: &Path, args: &[OsString]) -> Vec<OsString> {
    let before: Vec<OsString> = match terminal {
        // La ventana la abre gnome-terminal-server, con el directorio que se le pase.
        "gnome-terminal" => {
            let mut dir = OsString::from("--working-directory=");
            dir.push(cwd);
            vec!["--wait".into(), dir, "--".into()]
        }
        "ptyxis" => vec!["--new-window".into(), "--".into()],
        "konsole" => vec!["--nofork".into(), "-e".into()],
        "xfce4-terminal" => vec!["--disable-server".into(), "-x".into()],
        "mate-terminal" => vec!["-x".into()],
        "wezterm" => vec!["start".into(), "--always-new-process".into(), "--".into()],
        "kitty" | "foot" => vec![],
        // xterm, alacritty, x-terminal-emulator y la mayoría.
        _ => vec!["-e".into()],
    };
    before
        .into_iter()
        .chain([exe.as_os_str().to_owned()])
        .chain(args.iter().cloned())
        .collect()
}

/// La ventana usa su terminal, y solo si se la dio una terminal en el escritorio: la entrada y
/// la salida son esa terminal, el proceso abrió su propia sesión (no lo lanzó una shell) y su
/// padre es una terminal conocida (no `script`, `tmux` ni el agente).
pub fn claim_console() -> Result<(), String> {
    let refused = |detail: String| {
        tr!(
            "✗ `--window` is only for the window that `crabfall run --ask` opens{}.",
            "✗ `--window` es solo para la ventana que abre `crabfall run --ask`{}.",
            detail
        )
    };
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Err(refused(String::new()));
    }
    let me = Stat::of(std::process::id()).ok_or_else(|| refused(String::new()))?;
    if me.session != me.pid {
        return Err(refused(String::new()));
    }
    let parent = process_name(me.ppid).unwrap_or_default();
    if !terminal_parent(&parent, std::env::var("TERMINAL").ok().as_deref()) {
        return Err(refused(tr!(
            " (its parent, `{parent}`, is not a known terminal; set TERMINAL to yours)",
            " (su padre, `{parent}`, no es una terminal conocida; definí TERMINAL con la tuya)"
        )));
    }
    // El título de la ventana (OSC 0).
    eprint!(
        "\x1b]0;{}\x07",
        pick(
            "Crabfall · confirm the run",
            "Crabfall · confirmá la corrida"
        )
    );
    Ok(())
}

fn terminal_parent(parent: &str, terminal_env: Option<&str>) -> bool {
    if KNOWN_PARENTS.contains(&parent) {
        return true;
    }
    let preferred = terminal_env
        .and_then(|t| t.split_whitespace().next())
        .map(|t| file_name(Path::new(t)));
    preferred.is_some_and(|t| t == parent && !NOT_A_WINDOW.contains(&parent))
}

/// Lo que importa de `/proc/<pid>/stat`.
#[derive(Debug, PartialEq)]
struct Stat {
    pid: u32,
    state: char,
    ppid: u32,
    session: u32,
}

impl Stat {
    fn of(pid: u32) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    /// `pid (comm) state ppid pgrp session ...`; el nombre puede tener espacios y paréntesis.
    fn parse(line: &str) -> Option<Self> {
        let (pid, rest) = line.split_once(" (")?;
        let (_, rest) = rest.rsplit_once(") ")?;
        let mut fields = rest.split_whitespace();
        let state = fields.next()?.chars().next()?;
        let ppid = fields.next()?.parse().ok()?;
        let _pgrp = fields.next()?;
        let session = fields.next()?.parse().ok()?;
        Some(Self {
            pid: pid.trim().parse().ok()?,
            state,
            ppid,
            session,
        })
    }
}

/// Si el proceso existe y no terminó (un zombie ya terminó: falta que su padre lo recoja).
fn alive(pid: u32) -> bool {
    Stat::of(pid).is_some_and(|s| s.state != 'Z' && s.state != 'X')
}

/// El nombre del ejecutable de un proceso (`/proc/<pid>/exe`), o el de `comm` si no se puede leer.
fn process_name(pid: u32) -> Option<String> {
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(exe) => Some(file_name(&exe)),
        Err(_) => std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|c| c.trim().to_owned()),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(OsStr::to_string_lossy)
        .unwrap_or_default()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(terminal: &str) -> Vec<String> {
        launch_args(
            terminal,
            Path::new("/repo"),
            Path::new("/bin/crabfall"),
            &["run".into(), "x".into()],
        )
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect()
    }

    #[test]
    fn each_terminal_runs_the_command() {
        assert_eq!(args("xterm"), ["-e", "/bin/crabfall", "run", "x"]);
        assert_eq!(
            args("gnome-terminal"),
            [
                "--wait",
                "--working-directory=/repo",
                "--",
                "/bin/crabfall",
                "run",
                "x"
            ]
        );
        assert_eq!(args("kitty"), ["/bin/crabfall", "run", "x"]);
        assert_eq!(args("konsole")[..2], ["--nofork", "-e"]);
        assert_eq!(args("something-else")[0], "-e");
    }

    #[test]
    fn stat_with_spaces_and_parentheses_in_the_name() {
        let line = "4242 (crab (fall) x) S 17 4242 4242 34816 4242 4194304 1 0";
        assert_eq!(
            Stat::parse(line),
            Some(Stat {
                pid: 4242,
                state: 'S',
                ppid: 17,
                session: 4242
            })
        );
        assert_eq!(Stat::parse("garbage"), None);
    }

    #[test]
    fn only_terminals_can_be_the_parent() {
        assert!(terminal_parent("xterm", None));
        assert!(terminal_parent("gnome-terminal-server", None));
        assert!(!terminal_parent("script", None));
        assert!(!terminal_parent("bash", None));
        // `$TERMINAL` suma la suya, pero no una shell ni `script`.
        assert!(terminal_parent("cool-term", Some("/opt/cool-term --flag")));
        assert!(!terminal_parent("script", Some("script")));
        assert!(!terminal_parent("cool-term", Some("other")));
    }

    #[test]
    fn this_process_is_alive() {
        assert!(alive(std::process::id()));
        let me = Stat::of(std::process::id()).unwrap();
        assert_eq!(me.pid, std::process::id());
    }
}
