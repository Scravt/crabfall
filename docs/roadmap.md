# Roadmap — lt

Revisado 12 oct 2026. La versión anterior (con fases, fechas y OKRs) está en el historial de git.

## Objetivo

Pasar de "tengo una API" a "sé dónde se rompe y por qué" sin armar un proyecto por cada prueba. Claude diseña el escenario y analiza los resultados; una persona aprueba y ejecuta la carga.

## Principios

1. **Claude diseña y analiza; la persona autoriza la carga.** Claude solo puede ejecutar el smoke check contra loopback (ADR-004).
2. **Números confiables.** Sin coordinated omission, percentiles con HdrHistogram y aviso cuando el generador es el cuello de botella.
3. **Hallazgos con evidencia.** El motor detecta patrones de forma determinista; las hipótesis de Claude sobre el código se marcan como hipótesis.
4. **Un núcleo, interfaces delgadas.** CLI primero; MCP y GUI después (ADR-001).
5. **Local-first.** Escenarios YAML versionados en el repo del proyecto. Sin cuentas ni nube.
6. **Uso propio primero.** Algo está terminado cuando se usa en un caso real.
7. **Todo ajuste de una corrida entra por flag.** Lo que tendría sentido en un panel de personalización (carga, pausas, datos, entorno, variables) se puede pasar como `--algo` a `validate`, `check`, `approve` y `run`, con la misma lógica en `lt-core`. Así la GUI es una capa delgada (ADR-008).

## Cómo se usa

```
Vos:    "armame una prueba del checkout con 200 usuarios"
Claude: lee el código → escribe .loadtest/scenarios/checkout.yaml
        → lt validate --json → corrige → lt check checkout --json → corrige
Vos:    lt approve checkout --env staging
        lt run checkout --env staging
Claude: lt findings latest --json → lee el código de cada hallazgo → propone hipótesis
```

## Arquitectura

```
crates/
  lt-core      escenario, validación, hash, motor, métricas, diagnóstico
  lt-cli       binario `lt`
tools/
  lab-server   servidor axum con patologías configurables, para tests
integrations/claude/  skill de Claude Code (lo instala `lt claude install`)
```

`lt-core` se parte en crates más chicos cuando un módulo lo justifique, no antes.

En el repo del usuario:

```
.loadtest/
  config.yaml        entornos, allowed_hosts, limits, secrets
  scenarios/*.yaml   versionados
  approvals/         firmas locales (gitignored)
  runs/<id>/         resultados (gitignored)
```

## Rebanadas

Cada rebanada funciona de punta a punta antes de pasar a la siguiente. Al cerrarla se actualiza el README (estado, comandos y lo nuevo que cambia el uso); `crates/lt-cli/tests/readme.rs` falla si falta un comando o un ADR.

**0. Fundaciones**
- [x] Cargo workspace con `lt-core`, `lt-cli` y `lab-server`
- [x] Spike: cliente sobre `hyper` con timings por fase (DNS, connect, TTFB, receiving) → ADR-003 (TLS pendiente)
- [x] Spike: scheduler de tasa constante que mide desde el instante planificado → ADR-003
- [x] Elegir crate de YAML con línea y columna en los errores → `serde-saphyr` (ADR-002 §10)
- Listo cuando: 10.000 requests contra `lab-server` dan percentiles parecidos a `oha`, y la tasa se mantiene aunque el servidor se frene.

**1. Primera corrida**
- [x] Modelo mínimo: flows con peso, pasos sin `extract`, executor `constant_rate`, bodies `json`/`body`, HTTPS
- [x] `lt validate [--json]` con ruta exacta del error y `todo` pendientes
- [x] `lt run` → resumen en terminal y `results.json` con p50/p90/p95/p99 por paso
- [x] Guarda interina (reemplazada en la rebanada 3 por la aprobación firmada)
- Listo: probado contra `shop-api` (API local de ejemplo, repo aparte). **Pendiente:** TLS contra un servidor real, con `market-api` (Postgres, JWT, rate limit; repo aparte, 18 oct 2026) subida a un servidor.

**2. Claude diseña**
- [x] `lt check [--json]` según ADR-004
- [x] JSON Schema generado (`schemars`) y `lt schema`
- [x] Skill `loadtest` + `lt claude install` (ADR-001)
- Listo cuando: Claude arma desde el código un escenario que pasa `validate` y `check` sin que yo toque el YAML. Probado con `shop-api` (escenario `buscar-y-comprar`) siguiendo el skill en esta sesión. **Pendiente:** repetirlo en una sesión nueva de Claude Code, que carga el skill sola.

**3. Aprobación**
- [x] `lt approve` y la verificación en `lt run`, con firma, keyring y TTY (ADR-004)
- [x] `limits` y `allowed_hosts` en `config.yaml` (hecho en la rebanada 1)
- [x] Hook `PreToolUse` (`lt hook pre-tool-use`), adelantado a la rebanada 2
- [x] Tests de la guarda (lista en ADR-004; falta `--var`, que todavía no existe)
- Listo cuando: apruebo un escenario de `shop-api` en mi terminal, lo ejecuto, y cualquier edición posterior hace que `lt run` lo rechace mostrando el diff. **Pendiente de probar a mano:** la parte interactiva no se puede probar desde la shell de Claude, que no tiene terminal.

**4. Escenario completo**
- [x] `setup`, `extract` (json, header, regex), `defaults`, varios flows con `weight`
- [x] Interpolación con preservación de tipo y placeholders dinámicos, `{{env.X}}` para `secrets`, `--var`, `defaults`
- [x] Checks y thresholds
- [x] Executors `ramping_rate` (calendario exacto por tramos) y `constant_vus` (con pacer si hay `limits.max_rps`)
- [x] Códigos de salida: pasa, thresholds fallidos (1), error de configuración (2)
- Listo: `shop-api` hace login real en `setup`, encadena `token` y `order_id`, y verifica con checks y thresholds.

**5. Diagnóstico**
- [x] Series por segundo (iniciadas, descartadas, requests, errores, p50/p95/p99), globales y por paso, en `results.json`
- [x] Reglas: punto de quiebre, capacidad excedida, errores por umbral, degradación en el tiempo, cola larga, conexiones nuevas, resultado inválido (ADR-005)
- [x] `lt findings [--json]` con evidencia y `source` del paso; hallazgos al final de `lt run`
- [x] Skill: del hallazgo al código, separando lo medido de las hipótesis
- [x] Validar cada regla con una patología (servidores de prueba en `tests/diagnose.rs`)
- Listo: en `shop-api`, `login-ramp` da un `breaking_point` en ~36 req/s y Claude llega a `scryptSync` en `src/auth.ts` (15 oct 2026). `stress-ramp` no satura `shop-api` y no da hallazgos, como corresponde. La corrida destapó un falso positivo de `new_connections`, ya corregido (ADR-005).

**6. Comparar corridas**
- [x] `lt compare [base] [nueva] [--json]`: por defecto, la última contra la anterior del mismo escenario y entorno (ADR-006)
- [x] Veredicto por paso sobre el p95: magnitud (≥ 10% y 2 ms) y test de signos sobre los segundos emparejados, para separar cambio de ruido
- [x] Diferencias de totales, hallazgos (nuevos, resueltos, punto de quiebre que se mueve) y thresholds; sale con `1` si hay regresiones
- [x] Skill: sección "Comparar corridas"; `lt compare` permitido en `lt claude install`
- [x] Tests: unitarios, el motor contra un servidor rápido y uno lento, y el binario
- Listo: en `shop-api`, con `login` usando `scrypt` asincrónico (rama `fix/async-scrypt`), `login-ramp` sostiene los 80 req/s sin descartes ni hallazgos, y `lt compare` contra la corrida anterior da "mejoró": p95 de `login` 7213 → 32 ms en 25 de 29 segundos, el punto de quiebre y `new_connections` resueltos (16 oct 2026).

**7. Datasets**
- [x] `data:` en la raíz: CSV con encabezado (strings) o JSON (arreglo de objetos, conserva tipos), relativos a `.loadtest/` (ADR-007)
- [x] `{{dataset.columna}}`: una fila por iteración para todos sus pasos, `pick: sequential` (por defecto) o `random`; solo consumen filas los flows que usan el dataset
- [x] La aprobación firma el SHA-256 de cada archivo; `approve` y `run` muestran los datasets
- [x] Errores con la columna o la fila: archivo vacío o ilegible, columna inexistente (con las que hay), uso en `setup`
- [x] Skill: sección "Datos por iteración"
- [x] Tests: carga de CSV y JSON, orden de las filas, firma, errores y el motor contra el servidor de eco
- Listo: en `shop-api`, `login-users` hace login con un usuario distinto por iteración (`data/users.csv`) y pide productos al azar de `data/products.json`. La corrida aprobada (50 iteraciones, 100 requests) da 150 checks pasados y 0 fallidos, incluido `$.id == "{{products.id}}"`, que confirma que la fila llega con su tipo (17 oct 2026).

**8. Think time y ajustes por flag**
- [x] `think` en el paso y en `defaults`: fijo (`2s`) o al azar (`1s-3s`), después del paso; ocupa el VU, no cuenta en la latencia; `check` lo ignora y `setup` no lo permite (ADR-008)
- [x] Flags de ajuste en `validate`, `check`, `approve` y `run`: `--executor`, `--rate`, `--duration`, `--vus`, `--max-vus`, `--start-rate`, `--stages`, `--graceful-stop`, `--think`, `--data`, `--pick`
- [x] Los ajustes se vuelven a validar y el error dice qué flag falta o sobra; la aprobación los firma; `approve`, `run` y `results.json` los muestran
- [x] `Overrides` en `lt-core`, para que la GUI y MCP no dependan de la CLI
- [x] `--threshold "http.latency.p95<300ms"`: reemplaza a los thresholds del YAML; no cambia la firma (agregado después del cierre, 17 oct 2026)
- [x] Skill: pausas y ajustes por flag; README con ejemplos
- [x] Tests: unidades, aplicación de ajustes, firma, el motor con pausas y el binario
- Listo: en `shop-api`, `login-users --executor constant_vus --vus 5 --duration 20s --think 1s-2s` da 1,8 iteraciones por segundo (36 en 20 s; se esperaban ~1,7), 108 checks pasados y 0 fallidos, y el p50 de `login` en 34 ms, igual que sin pausas: la pausa no entra en la latencia. `results.json` guarda los ajustes en `tweaks` y las pausas en `load.think` (17 oct 2026).

**9. Reporte HTML**
- [x] `report.html` autocontenido por corrida (sin CDN; se abre sin conexión), generado por `lt run`; `lt report [corrida] [--open]` lo regenera (ADR-009)
- [x] Encabezado con veredicto, carga, pausas y ajustes; indicadores; hallazgos con evidencia y ventana sombreada; comparación con la corrida anterior y p95 antes/después con la misma carga
- [x] Gráficos por segundo con un solo eje, crosshair y tooltip (también con teclado), escala log opcional para la latencia, p95 por paso, modo claro y oscuro, tablas como vista sin gráficos
- [x] Datos embebidos sin poder cerrar el `<script>`; texto siempre con `textContent`
- [x] Verificado con Edge headless sobre las corridas reales de `shop-api` (capturas en claro y oscuro, mouse simulado por DevTools)
- Listo: revisé los reportes de las corridas de `shop-api` (el quiebre de `login-ramp` y el antes y después del arreglo) y se entiende qué pasó sin mirar la terminal (17 oct 2026). Se va a ajustar cuando se integre con la GUI.

**10. Setup por VU**
- [x] `vu_setup:` en la raíz: cada VU corre esos pasos una vez antes de la carga; lo extraído queda como variable del VU (ADR-010)
- [x] Una fila de cada dataset por VU; si `vu_setup` usa un dataset, los flows ven la fila de su VU
- [x] Preparación antes de la carga para `vus` o `max_vus`, con hasta 50 a la vez y respetando `limits.max_rps`; el modelo abierto reparte VUs desde un pool
- [x] VUs que fallan quedan afuera con aviso; si fallan todos, se aborta; `lt check` lo corre una vez
- [x] Firma solo si existe; `results.json`, el resumen de `run`, `approve` y el reporte lo muestran aparte de las métricas de carga
- [x] Tests: un login por VU con `constant_vus` y con el modelo abierto, el token correcto en cada iteración, falla total, validación y firma
- Listo: en `shop-api`, `orders-per-user` (3 VUs, uno por usuario de `data/users.csv`) hizo 3 logins antes de la carga (p50 34 ms) y después 22 iteraciones de compra con pausas de 1-2 s: 44 requests, 88 checks pasados y 0 fallidos, así que cada pedido quedó a nombre del usuario de su VU (18 oct 2026).

**11. Renovación de sesión**
- [x] `vu_refresh:` en la raíz (`every` y `steps` opcionales): renueva la sesión de cada VU antes de que venza el token; sin `steps`, repite `vu_setup` (ADR-011)
- [x] En segundo plano, esté el VU libre o en una iteración: la iteración en curso sigue con el token anterior y la siguiente toma el nuevo, así renovar no le saca capacidad a la carga
- [x] Un VU que no puede renovar deja de iterar, con aviso; `lt check` corre la renovación una vez
- [x] `--refresh-every` por flag; firma solo si existe; `results.json`, `approve`, el resumen de `run` y el reporte lo muestran aparte
- [x] Tests: renovación en los dos modelos (también los VUs libres del pool), el token nuevo en las iteraciones siguientes, falla de la renovación, validación, firma y flag
- [x] Diagnóstico: 401 a mitad de corrida se marcan como sesión vencida, con la salida según el escenario; arreglados el doble conteo de errores con `extract` y el "desde el inicio" falso con pocas requests por segundo
- [x] `approve` sugiere el `lt run` con los mismos ajustes
- Listo: en `market-api`, con tokens de 60 s y 20 VUs durante 2 minutos, `shop-per-user` recibió 608 401 desde el segundo 59 y el hallazgo dijo "venció la sesión" con la sugerencia de `vu_refresh`. `session-refresh`, lo mismo con renovación por `/auth/refresh` cada 45 s, hizo 40 renovaciones (p50 5,5 ms), 0 401 y 1.860 de 1.860 checks pasados (18 oct 2026).

**12. Subir archivos (multipart)**
- [x] `multipart:` en el `request`: campos de texto con variables y archivos de `.loadtest/` con `filename` y `content_type` opcionales; excluye a `json` y `body` (ADR-012)
- [x] Los archivos se leen una vez y van enteros en cada request (hasta 100 MB); boundary aleatorio por paso que no aparece en ningún archivo; `content-type` con el boundary lo pone lt
- [x] Firma el SHA-256 de cada archivo, no el boundary; `approve` y `run` muestran cada archivo con su tamaño y el paso que lo sube
- [x] Tests: un servidor con el parser multipart de axum recibe el archivo intacto (con CRLF y algo que parece un boundary) y los campos de texto; firma estable y sensible al contenido; validación
- Listo cuando: en `market-api`, `upload` (10 VUs, cada uno sube un adjunto de 1 MB, verifica el `sha256` que devuelve el servidor, lo baja y lo borra) termina con 0 checks fallidos. `check` pasa (18 oct 2026). **Pendiente:** la corrida (la aprueba y ejecuta una persona).

## Backlog (sin orden)

Bytes enviados y recibidos por segundo en los resultados (con archivos grandes, el ancho de banda puede ser el límite), más reglas de diagnóstico, corrida base fija para CI (`lt compare --baseline`), servidor MCP, importadores (OpenAPI, curl, Postman), GUI con Tauri, HTTP/2, `pick: unique` para datasets, leer `expires_in` para elegir solo cuándo renovar, leer `Retry-After` en los 429 para que el hallazgo diga la ventana del límite, GitHub Action, correlación con métricas del servidor (OTLP, Prometheus), WebSockets, gRPC, Kafka.

## Decisiones abiertas

| # | Pregunta | Recomendación actual |
| --- | --- | --- |
| D2 | Nombre del proyecto y del binario | Corto, pronunciable en inglés, libre en crates.io y GitHub |
| D4 | ¿El diagnóstico usa métricas del servidor? | No por ahora: solo lado cliente |

## Registro de decisiones

| Fecha | Decisión | Dónde |
| --- | --- | --- |
| 7 oct 2026 | Claude diseña y analiza; nunca dispara carga | Principio 1 |
| 7 oct 2026 | `hyper` directo en lugar de `reqwest` (confirmado con el spike el 12 oct) | ADR-003 |
| 7 oct 2026 | Diagnóstico en dos capas: reglas en el motor, hipótesis en Claude | Principio 3 |
| 10 oct 2026 | Claude puede hacer smoke checks solo contra loopback | ADR-004 |
| 10 oct 2026 | Formato de escenario YAML declarativo, aprobación derivada | ADR-002 |
| 12 oct 2026 | Claude-first: CLI con JSON + skill y hook de Claude Code (`lt claude install`); MCP e importadores después | ADR-001 |
| 12 oct 2026 | Trabajo por rebanadas verticales, sin fechas | Este documento |
| 12 oct 2026 | Toolchain Rust MSVC en Windows | — |
| 12 oct 2026 | Scheduler de modelo abierto: latencia desde el instante planificado, iteraciones descartadas cuando no hay VU | ADR-003 |
| 12 oct 2026 | En Windows se sube la resolución del timer a 1 ms | ADR-003 |
| 12 oct 2026 | YAML con `serde-saphyr` + `serde_path_to_error` (D5) | ADR-002 §10 |
| 12 oct 2026 | La aprobación firma el plan canónico resuelto; `approve` exige terminal siempre; `run` solo verifica la firma; sin vencimiento | ADR-004 |
| 12 oct 2026 | Cada trabajo en su rama, merge `--no-ff` a `dev`; `main` queda intacto hasta decidirlo | — |
| 12 oct 2026 | Checks y thresholds no se firman (no cambian el tráfico); `setup`, `extract` y variables sí | ADR-004 |
| 12 oct 2026 | `ramping_rate` con calendario exacto; `constant_vus` con pacer si hay `max_rps` | ADR-003 |
| 12 oct 2026 | Diagnóstico en dos capas: reglas deterministas con evidencia en el motor, hipótesis sobre el código en Claude | ADR-005 |
| 15 oct 2026 | `new_connections` descuenta una conexión por VU (el pool crece hasta la concurrencia) | ADR-005 |
| 15 oct 2026 | `lt compare`: veredicto por paso con magnitud + test de signos sobre segundos emparejados; regresiones salen con código 1 | ADR-006 |
| 16 oct 2026 | Datasets: `{{dataset.columna}}`, una fila por iteración, CSV como strings y JSON con tipos; se firma el SHA-256 de cada archivo | ADR-007 |
| 17 oct 2026 | Think time después de cada paso; todo ajuste de una corrida entra por flag y se firma | ADR-008 |
| 17 oct 2026 | Reporte HTML autocontenido por corrida, generado por `lt run` | ADR-009 |
| 17 oct 2026 | `vu_setup`: cada VU hace su login antes de la carga; el modelo abierto reparte VUs desde un pool | ADR-010 |
| 18 oct 2026 | `vu_refresh`: renovación en segundo plano, sin sacar al VU del pool; si falla, el VU deja de iterar | ADR-011 |
| 18 oct 2026 | `multipart`: archivos leídos una vez y firmados por contenido; sin streaming ni rutas con variables | ADR-012 |
