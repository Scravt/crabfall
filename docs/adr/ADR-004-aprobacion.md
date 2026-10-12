# ADR-004 — Aprobación y modelo de amenaza

Estado: **aceptado** · 12 oct 2026

## Contexto

Principio 1: Claude diseña y analiza, pero solo una persona autoriza la carga. Claude Code tiene shell y puede editar cualquier archivo del repo, así que la guarda no puede depender de que Claude "se porte bien" ni de los permisos de Claude Code. Tiene que estar en el binario `lt`.

## Qué se protege y qué no

**Se protege contra:**

- que Claude ejecute carga por iniciativa propia o por error (`lt run` sin aprobación);
- que Claude modifique un escenario aprobado y la aprobación siga valiendo;
- que Claude deje el escenario intacto y redirija el entorno (por ejemplo, la `base_url` de `local` hacia producción);
- que Claude se apruebe a sí mismo (`lt approve` por pipe, `yes | lt approve`).

**No se protege contra** un agente que actúa de forma deliberadamente maliciosa con los permisos del usuario. Ese agente puede generar carga sin `lt` (un loop de `curl`), emular una terminal o leer el keyring. La guarda evita accidentes y el mal uso de esta herramienta; no reemplaza la confianza en el agente ni la seguridad de la máquina.

## Decisión

### 1. Qué se firma

La aprobación es una firma HMAC-SHA256 sobre:

- el hash canónico del escenario (ADR-002, sin `description` ni `meta`);
- el **entorno resuelto**: base URL, `allowed_hosts`, variables (incluidas las de `--var`) y `limits`;
- la versión del formato.

La clave HMAC se genera en el primer `lt approve` y se guarda en el keyring del sistema (Windows Credential Manager, Keychain en macOS, Secret Service en Linux), nunca en el repo.

Las aprobaciones se guardan en `.loadtest/approvals/<escenario>.<entorno>.json` y no se versionan, porque la clave es local de cada máquina.

### 2. `lt approve <escenario> --env <entorno>`

- Exige una **terminal interactiva real**: si stdin o stdout no son una consola, falla.
- Muestra el resumen de riesgo: hosts destino, tasa máxima de requests, VUs, duración, pasos que escriben (`[write]`), y el diff contra la última aprobación.
- Rechaza si el escenario supera los `limits` del entorno o tiene `todo` pendientes.
- Pide que la persona **tipee el host destino** (no alcanza con "y").

### 3. `lt run <escenario> --env <entorno>`

- Recalcula el hash del escenario y del entorno resuelto, y verifica la firma. Si no coincide, falla con un mensaje claro y sale con un código propio.
- Vuelve a aplicar los `limits` en tiempo de ejecución: si el motor se acerca a superarlos, corta.
- Todo host al que se conecte, incluidas las redirecciones, tiene que estar en `allowed_hosts`.

### 4. `lt check` (smoke check, sin aprobación)

Es lo único que Claude puede ejecutar por su cuenta. Reglas:

- Todos los hosts tienen que resolver a loopback (`127.0.0.0/8` o `::1`); si alguno no, falla antes de enviar nada.
- No sigue redirecciones fuera de loopback.
- Corre `setup` y cada flow **una vez**, en serie, con 1 VU; ignora `load`.
- Tiene un tope total de requests (50) y un timeout por request.
- La salida trunca los cuerpos de respuesta (2 KB) y redacta los secretos.

### 5. Defensa adicional en Claude Code

El plugin (ADR-001) incluye un hook `PreToolUse` que bloquea `lt approve` y `lt run` cuando los invoca Claude. Es una capa extra contra errores, no la guarda principal.

## Verificación

Tests automáticos que tienen que fallar (no habilitar `run`):

- editar el YAML después de aprobar;
- cambiar `base_url`, `allowed_hosts`, `limits` o una variable del entorno;
- pasar un `--var` distinto del aprobado;
- editar o copiar a mano un archivo de `approvals/`;
- ejecutar `approve` con stdin redirigido (`echo host | lt approve ...`);
- `check` contra un host que no es loopback, o con una redirección hacia afuera.

## Consecuencias

- Toda ejecución de carga pasa por una acción humana visible, con el destino tipeado.
- Cada cambio, aunque sea mínimo, obliga a aprobar de nuevo. Es intencional.
- Aprobar desde CI no está contemplado; si hace falta, va a requerir un mecanismo aparte (por ejemplo, una clave en un secreto de CI) y su propio ADR.
