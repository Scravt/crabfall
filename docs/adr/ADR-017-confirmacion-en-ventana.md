# ADR-017 — El agente pide la corrida, la persona la confirma en una ventana

Estado: **aceptado** · 23 oct 2026 · amplía [ADR-004](ADR-004-aprobacion.md)

## Contexto

Con ADR-004, Claude diseña el escenario y la persona tiene que ir a su terminal para aprobar y correr cada vez. Eso corta el ciclo que más sirve: correr, comparar contra la base, tocar el código de la API y volver a correr. La persona quiere que el agente pueda lanzar las corridas mientras trabaja, pero sin perder lo que pide el principio 1: **ninguna carga sale sin una persona**.

Hoy `crabfall run` no exige terminal (ADR-004: una aprobación hecha por una persona se puede ejecutar desde un script); lo único que frena al agente es el hook. Y las aprobaciones no vencen. Si se destrabara `run` sin más, el agente podría repetir corridas sin que nadie confirme nada.

## Decisión

### 1. `crabfall run --ask`

- **Sin terminal** (lo lanzó un agente): Crabfall se relanza a sí mismo en una **consola nueva** (`CREATE_NEW_CONSOLE` en Windows) con `--window <archivo>` y espera. La ventana muestra el plan, pide la confirmación, **corre la carga ahí** con su progreso, y al terminar deja en el archivo el código de salida y dónde quedaron los resultados. El proceso del agente imprime eso y sale con el mismo código.
- **En una terminal** (una persona escribió `--ask`): pregunta ahí mismo.
- Antes de abrir la ventana se arma y valida el plan y se aplican los `limits`: los errores le llegan al agente sin molestar a la persona.

Que la carga corra en la ventana da un **freno de emergencia** que no depende del agente: Ctrl+C la frena en orden, como en `run` (ADR-015). Al terminar, la ventana muestra el resumen y se cierra sola a los 30 s (o con Enter).

### 2. Qué se pregunta

- **Sin aprobación vigente** (primera vez, o el plan cambió): el resumen de riesgo con el diff, y tipear el host, igual que `approve`. Firma la aprobación.
- **Con aprobación vigente**: el resumen y una respuesta corta, `s`/`y` para esta corrida. El agente no puede tipear en la ventana, así que alcanza; tipear el host en cada corrida cansa y termina en tipear sin leer.
- En los dos casos, la persona puede responder **minutos (1 a 120)**: permite el mismo plan sin preguntar durante ese tiempo. Si nadie responde en 5 min, se cancela.

### 3. El permiso por tiempo

`.crabfall/approvals/<escenario>.<entorno>.allow.json`, firmado con la misma clave HMAC sobre `crabfall-allowance/<formato>`, escenario, entorno, hash del plan aprobado y la hora de vencimiento.

- Vale solo para ese hash: cualquier cambio en el plan lo invalida y vuelve a preguntar con el diff.
- Es por tiempo y no por cantidad de corridas: copiar un archivo viejo no devuelve corridas, porque la hora firmada no cambia.
- Durante el permiso, la ventana se abre igual: la persona ve cada corrida y la puede frenar.
- Se revoca borrando el archivo.

### 4. La ventana se defiende

`--window` solo funciona si la consola es **nueva y visible**: el proceso es el único en ella (`GetConsoleProcessList`) y la ventana se ve (`IsWindowVisible`). En la consola del agente o en una terminal compartida, rechaza con código 3 sin preguntar nada. `--ask` y `--window` juntos son un error de uso.

### 5. Hook y skill

El hook deja pasar `crabfall run ... --ask` y sigue bloqueando `run` sin `--ask`, `run --window`, `approve` y la TUI. La skill le dice a Claude que use `--ask` en segundo plano, que espere a la persona, y que lea el resultado con `crabfall findings --json` y `crabfall compare --baseline --json`. La base la sigue moviendo solo la persona.

### 6. Otros sistemas

Por ahora, solo Windows. En macOS (Terminal vía `osascript`) y Linux (`$TERMINAL`, `x-terminal-emulator`) `--ask` sin terminal rechaza y pide que la persona corra el comando. Sin escritorio (SSH, Claude Code en la web) no hay dónde abrir la ventana: mismo rechazo.

## Qué no cambia: el agente malicioso

ADR-004 no protege contra un agente que actúa de forma deliberadamente maliciosa con los permisos del usuario, y esto tampoco. Lo verificamos:

- **Pulsaciones sintéticas.** UIPI solo impide inyectar input en procesos de *mayor* integridad; entre procesos del mismo usuario y nivel, `SendInput` funciona ([SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)). El agente puede tipear en la ventana, o lanzar `crabfall` bajo su propia ConPTY. La verificación de §4 sube el costo, pero no lo impide.
- **La clave.** Cualquier proceso del usuario lee el Credential Manager de ese usuario ([CredRead](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credreadw)): puede firmar aprobaciones y permisos.
- **Más simple todavía:** parchear el binario, o generar carga sin Crabfall (un loop de `curl`).

Cualquier verificación dentro de un proceso que el agente controla se puede saltear. Solo resiste lo que queda **fuera de su alcance**. Opciones, de menor a mayor costo:

1. **Sandbox del agente** (lo recomendado a corto plazo, sin código nuevo). El sandbox de Claude Code (macOS, Linux y WSL2; en Windows nativo no hay) restringe la red con una lista de hosts permitidos y puede negar lecturas (`denyRead` sobre la configuración y el keyring), con `failIfUnavailable` para no caer en silencio a "sin sandbox" ([sandboxing](https://code.claude.com/docs/en/sandboxing)). Si el agente no llega al target ni a la clave, solo `crabfall`, lanzado fuera del sandbox, genera carga. Un devcontainer o una VM con egress restringido logran lo mismo.
2. **El target exige un token de vida corta** (lo más robusto). Caddy (`forward_auth`) solo acepta tráfico con un token emitido después de una confirmación fuera de la máquina del agente: una llave FIDO2 (toque físico, clave asimétrica; la máquina guarda solo la pública) o un teléfono. Sirve solo para targets propios, como `market-api`, y el verificador tiene que vivir en otro host o usuario.
3. **Un broker con otro usuario del sistema** que guarde la clave y sea el único con salida de red hacia el target. Resiste a un agente sin admin, pero cuesta instalarlo y no es multiplataforma. UAC no sirve como frontera: Microsoft no lo considera un límite de seguridad.

Lo que **no** alcanza por sí solo: Windows Hello o Touch ID como "sí/no" (el resultado es un booleano que el agente puede saltear parcheando el binario), TOTP con el secreto en la misma máquina, o la elicitación de MCP (la media el propio cliente).

## Consecuencias

- El agente puede iterar (correr, comparar, cambiar el código) y la persona solo confirma. Con el permiso por tiempo, ni eso, pero sigue viendo cada corrida y puede frenarla.
- Una ventana que aparece puede quedar detrás de otra: la consola suena (campana) y titila en la barra de tareas.
- Las corridas del agente quedan en el historial como cualquier otra; `results.json` guarda la aprobación con la que corrieron.
- El modelo de amenaza es el de ADR-004: errores y exceso de iniciativa del agente, no un agente hostil. Para eso, el sandbox (opción 1) y, en targets propios, el token del servidor (opción 2) quedan en el backlog.
