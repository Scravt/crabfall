//! `crabfall run --ask` (ADR-017): un agente pide una corrida y una persona la confirma en
//! una ventana aparte, donde corre la carga y se puede frenar.
//!
//! Sin terminal (lo lanzó un agente), `run --ask` se relanza en una consola nueva con
//! `--window <archivo>` y espera. El hijo pide la confirmación, corre y deja en el archivo el
//! código de salida y dónde quedaron los resultados. En una terminal pregunta ahí mismo.

use std::{
    ffi::OsString,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        mpsc::{Receiver, channel},
    },
    time::Duration,
};

use crabfall_core::{
    approval::{self, Approval, OsKeyring},
    config::Environment,
    engine::plan::Plan,
    i18n::pick,
    project::Project,
    tr,
};
use serde_json::json;

use crate::{
    approve::{self, Change},
    display, exit,
    run::Outcome,
};

/// Cuánto espera la ventana una respuesta antes de cancelar.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Cuánto queda abierta la ventana al terminar, para leer el resultado.
const CLOSE_AFTER: Duration = Duration::from_secs(30);
/// El permiso más largo que se puede dar desde la ventana, en minutos.
const MAX_MINUTES: u64 = 120;

/// Del lado del agente: abre la ventana, espera y cuenta qué pasó.
pub fn delegate(as_json: bool) -> u8 {
    let file = std::env::temp_dir().join(format!(
        "crabfall-ask-{}-{}.json",
        std::process::id(),
        jiff::Timestamp::now().as_nanosecond()
    ));
    let mut args: Vec<OsString> = std::env::args_os()
        .skip(1)
        .filter(|a| a != "--ask" && a != "--json")
        .collect();
    args.push("--window".into());
    args.push(file.clone().into());

    eprintln!(
        "{}",
        tr!(
            "→ a window opened for the person to confirm (up to {} min to answer). \
             The load runs in that window and can be stopped there; waiting…",
            "→ se abrió una ventana para que la persona confirme (hasta {} min para responder). \
             La carga corre en esa ventana y se puede frenar ahí; esperando…",
            ANSWER_TIMEOUT.as_secs() / 60
        )
    );
    if let Err(e) = open_window(&args) {
        eprintln!(
            "{}",
            tr!(
                "✗ could not open the confirmation window: {e}",
                "✗ no se pudo abrir la ventana de confirmación: {e}"
            )
        );
        return exit::REFUSED;
    }

    let answer = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let _ = std::fs::remove_file(&file);
    let Some(answer) = answer else {
        eprintln!(
            "{}",
            tr!(
                "✗ the window closed without an answer: nothing ran.",
                "✗ la ventana se cerró sin respuesta: no corrió nada."
            )
        );
        return exit::REFUSED;
    };
    if let Some(note) = answer["note"].as_str() {
        eprintln!("{note}");
    }
    let path = |key: &str| answer[key].as_str().map(PathBuf::from);
    if let Some(results) = path("results") {
        eprintln!(
            "{}",
            tr!("results: {}", "resultados: {}", display(&results))
        );
        if let Some(report) = path("report") {
            eprintln!("{}", tr!("report:  {}", "reporte:    {}", display(&report)));
        }
        if as_json {
            if let Ok(doc) = std::fs::read_to_string(&results) {
                println!("{doc}");
            }
        } else {
            eprintln!(
                "{}",
                tr!(
                    "Summary: crabfall findings --json · against the baseline: crabfall compare --baseline --json",
                    "Resumen: crabfall findings --json · contra la base: crabfall compare --baseline --json"
                )
            );
        }
    }
    answer["code"].as_u64().map_or(exit::RUNTIME, |c| c as u8)
}

/// Del lado de la ventana: toma la consola, corre y deja la respuesta en `file`.
pub fn in_window(file: &Path, run: impl FnOnce() -> Outcome) -> u8 {
    let claimed = claim_console();
    let outcome = match &claimed {
        Ok(()) => {
            eprintln!(
                "{}",
                tr!(
                    "Crabfall · an agent asks to run load from {}\n",
                    "Crabfall · un agente pide correr carga desde {}\n",
                    std::env::current_dir().unwrap_or_default().display()
                )
            );
            run()
        }
        Err(why) => Outcome::refused(why.clone()),
    };
    let answer = json!({
        "code": outcome.code,
        "note": outcome.note,
        "results": outcome.results,
        "report": outcome.report,
    });
    std::fs::write(file, answer.to_string()).ok();
    if claimed.is_ok() {
        eprint!(
            "{}",
            tr!(
                "\nPress Enter to close (it closes on its own in {} s).",
                "\nEnter para cerrar (se cierra sola en {} s).",
                CLOSE_AFTER.as_secs()
            )
        );
        std::io::stderr().flush().ok();
        answer_line(CLOSE_AFTER);
    }
    outcome.code
}

/// Pide la confirmación de esta corrida a la persona. Devuelve la aprobación vigente, o
/// por qué no corre (para el agente).
pub fn confirm(
    project: &Project,
    plan: &Plan,
    env: &Environment,
    env_name: &str,
    tweaks: &[String],
) -> Result<Approval, String> {
    let now = jiff::Timestamp::now().as_second() as u64;
    if let Ok(current) = approve::verify(project, plan, env, env_name, tweaks) {
        if let Some(until) = allowed_until(project, &current, now) {
            crate::run::print_plan(plan, env_name, tweaks);
            eprintln!(
                "{}",
                tr!(
                    "  allowed without asking until {} · Ctrl+C stops it",
                    "  permitido sin preguntar hasta las {} · Ctrl+C la frena",
                    clock(until)
                )
            );
            return Ok(current);
        }
        for line in approve::summary(plan, env, env_name, &Change::Same, tweaks) {
            eprintln!("{line}");
        }
        prompt(&tr!(
            "\nType y to run it, or a number of minutes (1-{MAX_MINUTES}) to run it and allow \
             this same plan without asking for that long. Anything else cancels: ",
            "\nEscribí s para correrla, o una cantidad de minutos (1-{MAX_MINUTES}) para correrla \
             y permitir este mismo plan sin preguntar durante ese tiempo. Cualquier otra cosa cancela: "
        ));
        let answer = answer_line(ANSWER_TIMEOUT).ok_or_else(no_answer)?;
        let answer = answer.trim();
        if let Some(minutes) = minutes(answer) {
            grant(project, &current, now, minutes)?;
        } else if !matches!(
            answer.to_lowercase().as_str(),
            "y" | "s" | "yes" | "si" | "sí"
        ) {
            return Err(cancelled());
        }
        return Ok(current);
    }

    // Sin aprobación vigente: la de siempre, con el host tipeado.
    let (change, warning) = approve::change(project, plan, env, env_name);
    if let Some(w) = warning {
        eprintln!("{w}");
    }
    for line in approve::summary(plan, env, env_name, &change, tweaks) {
        eprintln!("{line}");
    }
    let target = approve::target_host(plan);
    prompt(&tr!(
        "\nIt needs an approval. To approve it and run it, type the target host ({target}): ",
        "\nNecesita una aprobación. Para aprobarla y correrla, escribí el host destino ({target}): "
    ));
    let typed = answer_line(ANSWER_TIMEOUT).ok_or_else(no_answer)?;
    if !typed.trim().eq_ignore_ascii_case(target) {
        return Err(tr!(
            "✗ the person did not type `{target}`: nothing was approved or run.",
            "✗ la persona no tipeó `{target}`: no se aprobó ni corrió nada."
        ));
    }
    let file = approve::sign(project, plan, env, env_name, tweaks)?;
    eprintln!(
        "{}",
        tr!("✓ approved: {}", "✓ aprobado: {}", display(&file))
    );
    let current =
        approve::verify(project, plan, env, env_name, tweaks).map_err(|l| l.join("\n"))?;
    prompt(&tr!(
        "Optional: to stop asking about this same plan, type how many minutes (1-{MAX_MINUTES}).\n\
         Enter runs the load now: ",
        "Opcional: para no volver a preguntar por este mismo plan, escribí cuántos minutos (1-{MAX_MINUTES}).\n\
         Enter corre la carga ahora: "
    ));
    if let Some(minutes) = answer_line(ANSWER_TIMEOUT)
        .as_deref()
        .and_then(|a| minutes(a.trim()))
    {
        grant(project, &current, now, minutes)?;
    }
    Ok(current)
}

/// Hasta cuándo deja correr `approval` sin preguntar el permiso guardado, si vale.
fn allowed_until(project: &Project, approval: &Approval, now: u64) -> Option<u64> {
    let file = approval::allowance_path(
        &project.approvals_dir(),
        &approval.scenario,
        &approval.environment,
    );
    let allowance: approval::Allowance =
        serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    approval::allows(&allowance, approval, now, &OsKeyring).then_some(allowance.until)
}

fn grant(project: &Project, approval: &Approval, now: u64, minutes: u64) -> Result<(), String> {
    let until = now + minutes * 60;
    let allowance = approval::allow(approval, until, &OsKeyring)
        .map_err(|e| tr!("could not sign: {e}", "no se pudo firmar: {e}"))?;
    let file = approval::allowance_path(
        &project.approvals_dir(),
        &approval.scenario,
        &approval.environment,
    );
    std::fs::write(&file, serde_json::to_string_pretty(&allowance).unwrap()).map_err(|e| {
        tr!(
            "could not save the allowance: {e}",
            "no se pudo guardar el permiso: {e}"
        )
    })?;
    eprintln!(
        "{}",
        tr!(
            "✓ allowed without asking until {} (delete {} to revoke it)",
            "✓ permitido sin preguntar hasta las {} (borrá {} para revocarlo)",
            clock(until),
            display(&file)
        )
    );
    Ok(())
}

/// `30` → 30 minutos, dentro del máximo.
fn minutes(answer: &str) -> Option<u64> {
    answer
        .parse::<u64>()
        .ok()
        .filter(|m| (1..=MAX_MINUTES).contains(m))
}

/// La hora local, `14:35`.
fn clock(unix: u64) -> String {
    jiff::Timestamp::from_second(unix as i64)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn prompt(text: &str) {
    // La campana hace titilar la ventana si quedó detrás de otra.
    eprint!("\x07{text}");
    std::io::stderr().flush().ok();
}

fn no_answer() -> String {
    tr!(
        "✗ nobody answered in {} min: nothing ran.",
        "✗ nadie respondió en {} min: no corrió nada.",
        ANSWER_TIMEOUT.as_secs() / 60
    )
}

fn cancelled() -> String {
    pick(
        "✗ the person cancelled the run.",
        "✗ la persona canceló la corrida.",
    )
    .into()
}

/// Una línea de la terminal, o `None` si no llega a tiempo. Un solo hilo lee stdin: si una
/// pregunta vence, la siguiente no compite con una lectura colgada.
fn answer_line(timeout: Duration) -> Option<String> {
    static LINES: OnceLock<Mutex<Receiver<String>>> = OnceLock::new();
    let lines = LINES.get_or_init(|| {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Mutex::new(rx)
    });
    lines.lock().unwrap().recv_timeout(timeout).ok()
}

#[cfg(windows)]
fn open_window(args: &[OsString]) -> std::io::Result<()> {
    use std::{os::windows::process::CommandExt, process::Stdio};
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    std::process::Command::new(std::env::current_exe()?)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_CONSOLE)
        .status()
        .map(|_| ())
}

#[cfg(not(windows))]
fn open_window(_: &[OsString]) -> std::io::Result<()> {
    Err(std::io::Error::other(pick(
        "for now it only works on Windows; ask the person to run it in their terminal",
        "por ahora solo funciona en Windows; pedile a la persona que lo corra en su terminal",
    )))
}

/// La ventana usa su propia consola, y solo si es nueva y visible: la abrió `delegate` para
/// esto. Una consola compartida (la de un agente o una terminal) no sirve para confirmar.
#[cfg(windows)]
fn claim_console() -> Result<(), String> {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::{
        System::Console::{
            GetConsoleProcessList, GetConsoleWindow, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
            STD_OUTPUT_HANDLE, SetConsoleTitleW, SetStdHandle,
        },
        UI::WindowsAndMessaging::IsWindowVisible,
    };
    let refused = || {
        tr!(
            "✗ `--window` is only for the window that `crabfall run --ask` opens.",
            "✗ `--window` es solo para la ventana que abre `crabfall run --ask`."
        )
    };
    let mut ids = [0u32; 4];
    // SAFETY: el buffer vive durante la llamada y su largo es el que se pasa.
    let attached = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
    // SAFETY: sin argumentos; devuelve null si no hay consola.
    let window = unsafe { GetConsoleWindow() };
    if attached != 1 || window.is_null() || unsafe { IsWindowVisible(window) } == 0 {
        return Err(refused());
    }
    let open = |name: &str| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .map_err(|_| refused())
    };
    let (input, output) = (open("CONIN$")?, open("CONOUT$")?);
    let error = output.try_clone().map_err(|_| refused())?;
    let title: Vec<u16> = pick(
        "Crabfall · confirm the run",
        "Crabfall · confirmá la corrida",
    )
    .encode_utf16()
    .chain([0])
    .collect();
    // SAFETY: los handles quedan abiertos para siempre (`into_raw_handle`) y el título termina en 0.
    unsafe {
        SetStdHandle(STD_INPUT_HANDLE, input.into_raw_handle());
        SetStdHandle(STD_OUTPUT_HANDLE, output.into_raw_handle());
        SetStdHandle(STD_ERROR_HANDLE, error.into_raw_handle());
        SetConsoleTitleW(title.as_ptr());
    }
    Ok(())
}

#[cfg(not(windows))]
fn claim_console() -> Result<(), String> {
    Err(tr!(
        "✗ `--window` is only for the window that `crabfall run --ask` opens, on Windows.",
        "✗ `--window` es solo para la ventana que abre `crabfall run --ask`, en Windows."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_within_the_maximum() {
        assert_eq!(minutes("30"), Some(30));
        assert_eq!(minutes("120"), Some(120));
        assert_eq!(minutes("0"), None);
        assert_eq!(minutes("121"), None);
        assert_eq!(minutes("s"), None);
    }
}
