# Roadmap — Crabfall

Revisado 12 oct 2026. La versión anterior (con fases, fechas y OKRs) está en el historial de git.

## Objetivo

Pasar de "tengo una API" a "sé dónde se rompe y por qué" sin armar un proyecto por cada prueba. Claude diseña el escenario y analiza los resultados; una persona aprueba y ejecuta la carga.

## Principios

1. **Claude diseña y analiza; la persona autoriza la carga.** Claude solo puede ejecutar el smoke check contra loopback (ADR-004).
2. **Números confiables.** Sin coordinated omission, percentiles con HdrHistogram y aviso cuando el generador es el cuello de botella.
3. **Hallazgos con evidencia.** El motor detecta patrones de forma determinista; las hipótesis de Claude sobre el código se marcan como hipótesis.
4. **Un núcleo, interfaces delgadas.** CLI primero; después una TUI en la terminal y MCP. La GUI gráfica (Tauri) queda para más adelante (ADR-001).
5. **Local-first.** Escenarios YAML versionados en el repo del proyecto. Sin cuentas ni nube.
6. **Uso propio primero.** Algo está terminado cuando se usa en un caso real.
7. **Todo ajuste de una corrida entra por flag.** Lo que tendría sentido en un panel de personalización (carga, pausas, datos, entorno, variables) se puede pasar como `--algo` a `validate`, `check`, `approve` y `run`, con la misma lógica en `crabfall-core`. Así la TUI (y más adelante una GUI) es una capa delgada (ADR-008).

## Cómo se usa

```
Vos:    "armame una prueba del checkout con 200 usuarios"
Claude: lee el código → escribe .crabfall/scenarios/checkout.yaml
        → crabfall validate --json → corrige → crabfall check checkout --json → corrige
Vos:    crabfall approve checkout --env staging
        crabfall run checkout --env staging
Claude: crabfall findings latest --json → lee el código de cada hallazgo → propone hipótesis
```

## Arquitectura

```
crates/
  crabfall-core      escenario, validación, hash, motor, métricas, diagnóstico
  crabfall-cli       binario `crabfall`
tools/
  lab-server   servidor axum con patologías configurables, para tests
integrations/claude/  skill de Claude Code (lo instala `crabfall claude install`)
```

`crabfall-core` se parte en crates más chicos cuando un módulo lo justifique, no antes.

En el repo del usuario:

```
.crabfall/
  config.yaml        entornos, allowed_hosts, limits, secrets
  scenarios/*.yaml   versionados
  approvals/         firmas locales (gitignored)
  runs/<id>/         resultados (gitignored)
```

## Rebanadas

Cada rebanada funciona de punta a punta antes de pasar a la siguiente. Al cerrarla se actualiza el README (estado, comandos y lo nuevo que cambia el uso); `crates/crabfall-cli/tests/readme.rs` falla si falta un comando o un ADR.

**0. Fundaciones**
- [x] Cargo workspace con `crabfall-core`, `crabfall-cli` y `lab-server`
- [x] Spike: cliente sobre `hyper` con timings por fase (DNS, connect, TTFB, receiving) → ADR-003 (TLS pendiente)
- [x] Spike: scheduler de tasa constante que mide desde el instante planificado → ADR-003
- [x] Elegir crate de YAML con línea y columna en los errores → `serde-saphyr` (ADR-002 §10)
- Listo cuando: 10.000 requests contra `lab-server` dan percentiles parecidos a `oha`, y la tasa se mantiene aunque el servidor se frene.

**1. Primera corrida**
- [x] Modelo mínimo: flows con peso, pasos sin `extract`, executor `constant_rate`, bodies `json`/`body`, HTTPS
- [x] `crabfall validate [--json]` con ruta exacta del error y `todo` pendientes
- [x] `crabfall run` → resumen en terminal y `results.json` con p50/p90/p95/p99 por paso
- [x] Guarda interina (reemplazada en la rebanada 3 por la aprobación firmada)
- Listo: probado contra `shop-api` (API local de ejemplo, repo aparte). **Pendiente:** TLS contra un servidor real, con `market-api` (Postgres, JWT, rate limit; repo aparte, 18 oct 2026) subida a un servidor.

**2. Claude diseña**
- [x] `crabfall check [--json]` según ADR-004
- [x] JSON Schema generado (`schemars`) y `crabfall schema`
- [x] Skill `crabfall` + `crabfall claude install` (ADR-001)
- Listo cuando: Claude arma desde el código un escenario que pasa `validate` y `check` sin que yo toque el YAML. Probado con `shop-api` (escenario `buscar-y-comprar`) siguiendo el skill en esta sesión. **Pendiente:** repetirlo en una sesión nueva de Claude Code, que carga el skill sola.

**3. Aprobación**
- [x] `crabfall approve` y la verificación en `crabfall run`, con firma, keyring y TTY (ADR-004)
- [x] `limits` y `allowed_hosts` en `config.yaml` (hecho en la rebanada 1)
- [x] Hook `PreToolUse` (`crabfall hook pre-tool-use`), adelantado a la rebanada 2
- [x] Tests de la guarda (lista en ADR-004; falta `--var`, que todavía no existe)
- Listo cuando: apruebo un escenario de `shop-api` en mi terminal, lo ejecuto, y cualquier edición posterior hace que `crabfall run` lo rechace mostrando el diff. **Pendiente de probar a mano:** la parte interactiva no se puede probar desde la shell de Claude, que no tiene terminal.

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
- [x] `crabfall findings [--json]` con evidencia y `source` del paso; hallazgos al final de `crabfall run`
- [x] Skill: del hallazgo al código, separando lo medido de las hipótesis
- [x] Validar cada regla con una patología (servidores de prueba en `tests/diagnose.rs`)
- Listo: en `shop-api`, `login-ramp` da un `breaking_point` en ~36 req/s y Claude llega a `scryptSync` en `src/auth.ts` (15 oct 2026). `stress-ramp` no satura `shop-api` y no da hallazgos, como corresponde. La corrida destapó un falso positivo de `new_connections`, ya corregido (ADR-005).

**6. Comparar corridas**
- [x] `crabfall compare [base] [nueva] [--json]`: por defecto, la última contra la anterior del mismo escenario y entorno (ADR-006)
- [x] Veredicto por paso sobre el p95: magnitud (≥ 10% y 2 ms) y test de signos sobre los segundos emparejados, para separar cambio de ruido
- [x] Diferencias de totales, hallazgos (nuevos, resueltos, punto de quiebre que se mueve) y thresholds; sale con `1` si hay regresiones
- [x] Skill: sección "Comparar corridas"; `crabfall compare` permitido en `crabfall claude install`
- [x] Tests: unitarios, el motor contra un servidor rápido y uno lento, y el binario
- Listo: en `shop-api`, con `login` usando `scrypt` asincrónico (rama `fix/async-scrypt`), `login-ramp` sostiene los 80 req/s sin descartes ni hallazgos, y `crabfall compare` contra la corrida anterior da "mejoró": p95 de `login` 7213 → 32 ms en 25 de 29 segundos, el punto de quiebre y `new_connections` resueltos (16 oct 2026).

**7. Datasets**
- [x] `data:` en la raíz: CSV con encabezado (strings) o JSON (arreglo de objetos, conserva tipos), relativos a `.crabfall/` (ADR-007)
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
- [x] `Overrides` en `crabfall-core`, para que la GUI y MCP no dependan de la CLI
- [x] `--threshold "http.latency.p95<300ms"`: reemplaza a los thresholds del YAML; no cambia la firma (agregado después del cierre, 17 oct 2026)
- [x] Skill: pausas y ajustes por flag; README con ejemplos
- [x] Tests: unidades, aplicación de ajustes, firma, el motor con pausas y el binario
- Listo: en `shop-api`, `login-users --executor constant_vus --vus 5 --duration 20s --think 1s-2s` da 1,8 iteraciones por segundo (36 en 20 s; se esperaban ~1,7), 108 checks pasados y 0 fallidos, y el p50 de `login` en 34 ms, igual que sin pausas: la pausa no entra en la latencia. `results.json` guarda los ajustes en `tweaks` y las pausas en `load.think` (17 oct 2026).

**9. Reporte HTML**
- [x] `report.html` autocontenido por corrida (sin CDN; se abre sin conexión), generado por `crabfall run`; `crabfall report [corrida] [--open]` lo regenera (ADR-009)
- [x] Encabezado con veredicto, carga, pausas y ajustes; indicadores; hallazgos con evidencia y ventana sombreada; comparación con la corrida anterior y p95 antes/después con la misma carga
- [x] Gráficos por segundo con un solo eje, crosshair y tooltip (también con teclado), escala log opcional para la latencia, p95 por paso, modo claro y oscuro, tablas como vista sin gráficos
- [x] Datos embebidos sin poder cerrar el `<script>`; texto siempre con `textContent`
- [x] Verificado con Edge headless sobre las corridas reales de `shop-api` (capturas en claro y oscuro, mouse simulado por DevTools)
- Listo: revisé los reportes de las corridas de `shop-api` (el quiebre de `login-ramp` y el antes y después del arreglo) y se entiende qué pasó sin mirar la terminal (17 oct 2026). Se va a ajustar cuando se integre con la GUI.

**10. Setup por VU**
- [x] `vu_setup:` en la raíz: cada VU corre esos pasos una vez antes de la carga; lo extraído queda como variable del VU (ADR-010)
- [x] Una fila de cada dataset por VU; si `vu_setup` usa un dataset, los flows ven la fila de su VU
- [x] Preparación antes de la carga para `vus` o `max_vus`, con hasta 50 a la vez y respetando `limits.max_rps`; el modelo abierto reparte VUs desde un pool
- [x] VUs que fallan quedan afuera con aviso; si fallan todos, se aborta; `crabfall check` lo corre una vez
- [x] Firma solo si existe; `results.json`, el resumen de `run`, `approve` y el reporte lo muestran aparte de las métricas de carga
- [x] Tests: un login por VU con `constant_vus` y con el modelo abierto, el token correcto en cada iteración, falla total, validación y firma
- Listo: en `shop-api`, `orders-per-user` (3 VUs, uno por usuario de `data/users.csv`) hizo 3 logins antes de la carga (p50 34 ms) y después 22 iteraciones de compra con pausas de 1-2 s: 44 requests, 88 checks pasados y 0 fallidos, así que cada pedido quedó a nombre del usuario de su VU (18 oct 2026).

**11. Renovación de sesión**
- [x] `vu_refresh:` en la raíz (`every` y `steps` opcionales): renueva la sesión de cada VU antes de que venza el token; sin `steps`, repite `vu_setup` (ADR-011)
- [x] En segundo plano, esté el VU libre o en una iteración: la iteración en curso sigue con el token anterior y la siguiente toma el nuevo, así renovar no le saca capacidad a la carga
- [x] Un VU que no puede renovar deja de iterar, con aviso; `crabfall check` corre la renovación una vez
- [x] `--refresh-every` por flag; firma solo si existe; `results.json`, `approve`, el resumen de `run` y el reporte lo muestran aparte
- [x] Tests: renovación en los dos modelos (también los VUs libres del pool), el token nuevo en las iteraciones siguientes, falla de la renovación, validación, firma y flag
- [x] Diagnóstico: 401 a mitad de corrida se marcan como sesión vencida, con la salida según el escenario; arreglados el doble conteo de errores con `extract` y el "desde el inicio" falso con pocas requests por segundo
- [x] `approve` sugiere el `crabfall run` con los mismos ajustes
- Listo: en `market-api`, con tokens de 60 s y 20 VUs durante 2 minutos, `shop-per-user` recibió 608 401 desde el segundo 59 y el hallazgo dijo "venció la sesión" con la sugerencia de `vu_refresh`. `session-refresh`, lo mismo con renovación por `/auth/refresh` cada 45 s, hizo 40 renovaciones (p50 5,5 ms), 0 401 y 1.860 de 1.860 checks pasados (18 oct 2026).

**12. Subir archivos (multipart)**
- [x] `multipart:` en el `request`: campos de texto con variables y archivos de `.crabfall/` con `filename` y `content_type` opcionales; excluye a `json` y `body` (ADR-012)
- [x] Los archivos se leen una vez y van enteros en cada request (hasta 100 MB); boundary aleatorio por paso que no aparece en ningún archivo; `content-type` con el boundary lo pone Crabfall
- [x] Firma el SHA-256 de cada archivo, no el boundary; `approve` y `run` muestran cada archivo con su tamaño y el paso que lo sube
- [x] Tests: un servidor con el parser multipart de axum recibe el archivo intacto (con CRLF y algo que parece un boundary) y los campos de texto; firma estable y sensible al contenido; validación
- Listo: en `market-api`, `upload` (10 VUs durante 1 minuto, cada uno sube un adjunto de 1 MB, verifica el `sha256` que devuelve el servidor, lo baja y lo borra) hizo 137 iteraciones y 411 requests sin fallas, con 685 de 685 checks pasados: los 137 archivos llegaron intactos. La subida tuvo p50 9 ms y p95 134 ms; bajar, p50 5 ms (19 oct 2026).

**13. Bytes por segundo**
- [x] Bytes del socket por request (headers, body y TLS), con un medidor por request enganchado a la conexión; cuentan aunque el request venza por timeout (ADR-013)
- [x] `bytes_sent` y `bytes_received` por paso, por segundo en la serie y en `totals`, con `send_rate` y `receive_rate`; `crabfall findings --json` los incluye
- [x] `crabfall run` muestra el total, el promedio y el pico por segundo (también en Mbit/s); el reporte suma un tile, un gráfico por segundo y los bytes por request de cada paso
- [x] Tests: un upload de 300 KB y una descarga de 256 KB medidos por paso (body más unos cientos de bytes de headers), la serie suma igual que los totales, y los bytes de un request que vence por timeout cuentan
- Listo: en `market-api`, `upload` (10 VUs durante 1 minuto, ya como `crabfall`) movió 137,1 MB enviados y 137,2 MB recibidos, 2,1 MB/s en cada dirección. Por request, `upload_file` mandó el archivo más 559 bytes de headers y multipart, y `download_file` recibió el archivo más 487. El pico, 10 MB/s (84 Mbit/s), fue el segundo 0, con los 10 VUs subiendo a la vez. 685 de 685 checks pasados (19 oct 2026).

**14. Cliente HTTP real**
- [x] Cookies: un jar por VU (RFC 6265, con `cookie_store`) desde `vu_setup` hasta la última iteración, también en `vu_refresh`; `setup` con el suyo; un header `cookie` propio se suma; `cookies: false` las apaga (ADR-014)
- [x] Redirects: hasta 10 por paso (`redirects` en el request o en `defaults`), solo hacia `allowed_hosts`; 303 y POST con 301/302 pasan a GET; `authorization` y la cookie propia no viajan a otro host; latencia, timeout y bytes cubren la cadena
- [x] Compresión: `accept-encoding: gzip, br` por defecto; checks y `extract` ven el body descomprimido (gzip, deflate, br; tope de 64 MB) y los bytes son los del cable
- [x] `form:` arma bodies `application/x-www-form-urlencoded`
- [x] `crabfall check` no sale de loopback ni por un redirect: el cliente se niega a conectarse afuera
- [x] Cada VU tiene su propia sesión aunque no haya `vu_setup` (antes compartían una vacía)
- [x] `approve` y `run` muestran una línea `HTTP`; `check` muestra cada salto de un redirect
- [x] CI en GitHub Actions: fmt y clippy en Linux, tests en Linux y Windows
- [x] Tests: contra axum con `tower-http` comprimiendo de verdad (gzip y br), sesión por VU, cookies apagadas, redirects (cadena, límite, loop, POST 302 y 307, otro host sin `authorization`), check sin salir de loopback, formularios, firma y validación
- Listo: en `market-api`, `browse` aprobado de nuevo (20 it/s durante 30 s) dio 1.800 de 1.800 checks, sin fallas, con `search` en p50 10 ms como antes. Recibió unos 2 KB por request: Express no comprime, así que pedir `gzip, br` no cambió nada, como corresponde (19 oct 2026). **Pendiente:** ver la compresión detrás de Caddy (perfil `tls`, `encode gzip`), junto con la prueba de TLS de la rebanada 1.

**15. Lo que dice el servidor**
- [x] De cada 429 se guardan `Retry-After` y la política declarada (`RateLimit-Policy` draft-8 y drafts 6-7, `X-RateLimit-Limit`) en `steps[].rate_limit`; `error_threshold` dice el límite con números; `crabfall run` lo muestra en una línea `límite` (ADR-005)
- [x] `crabfall compare` compara bytes por request y avisa si la respuesta o el request cambió mucho de tamaño (ADR-006)
- [x] Tests: el parser con los headers reales de express-rate-limit y los formatos viejos, la patología de rate limit con headers, y el aviso de tamaño (también con corridas sin bytes)
- Listo: en `market-api`, `rate-limit` (20 it/s contra `/me` durante 45 s) recibió 300 429 desde el segundo 30 (20/s × 30 s = 600). El hallazgo dice "declara 600 requests cada 60 s (`RateLimit-Policy`) y pide esperar con `Retry-After de 16 a 30 s`", y `crabfall run` lo muestra en la línea `límite` (19 oct 2026). La corrida destapó un detalle de formato, ya corregido: con poco tráfico el pico salía "0.0 Mbit/s"; ahora sale en kbit/s.

**16. Idioma** (ADR-016)
- [x] Inglés por defecto; `--lang` > `CRABFALL_LANG` > `language:` en config.yaml; sin tomar el idioma del sistema
- [x] Toda la salida en los dos idiomas: terminal, validación, plan, hallazgos, avisos, comparación, ayuda de clap y reporte HTML (con números y fechas según el idioma)
- [x] Cada mensaje con sus dos versiones juntas (`tr!`, `pick`); la ayuda se traduce con una tabla; el reporte con `L(en, es)`
- [x] README, skill y documentación del schema en inglés; ADRs y roadmap en español
- [x] Tests: los existentes en español con `CRABFALL_LANG=es` en `.cargo/config.toml`; los de inglés aparte (prioridad flag > variable > config, ayuda, idioma desconocido, mensajes del core)
- Listo cuando: en `market-api`, una corrida sin configurar sale en inglés, con `language: es` en el config sale en español, y el reporte acompaña. **Pendiente:** la corrida.

**17. Frenar y ver en vivo** (motor, para la TUI; ADR-015)
- [x] `Progress` lleva un pedido de freno de dos niveles: el primero deja de iniciar iteraciones y espera a las que están en curso (`graceful_stop`); el segundo corta ya
- [x] Frenar antes de la carga (en `setup` o `vu_setup`) aborta la corrida; durante `vu_setup` corta los logins en curso
- [x] La corrida frenada guarda `results.json` y el reporte con `stopped_at_s`, un aviso al principio y `passed: false`; el diagnóstico corre sobre lo que hubo; `crabfall compare` avisa que cubre menos
- [x] Ctrl+C en `crabfall run`: el primero frena en orden, el segundo corta
- [x] En vivo: fase (setup, vu_setup, carga, drenando, terminada), VUs ocupados, renovaciones fallidas y cada segundo cerrado de la serie (global y por paso), publicado medio segundo después de cerrar; la serie final y la publicada salen de la misma función
- [x] Tests: frenar en los dos modelos, dos veces (corta lo que está en curso), durante `vu_setup`, y la serie en vivo igual a la final
- Listo: en `market-api`, `browse --duration 2m` frenado con Ctrl+C a los 20 s terminó en 20,8 s (sin iteraciones largas que esperar), con 416 de 416 iteraciones y 1.248 checks pasados, y guardó resultados y reporte con el aviso "Stopped by the person at second 20.8". La corrida destapó dos detalles ya corregidos: la columna `no-2xx` sin traducir y dos líneas en blanco después del aviso (20 oct 2026).

**18. TUI** (ADR-015)
- [x] `crabfall` sin argumentos abre la TUI si hay terminal (si no, la ayuda); el hook la bloquea para Claude (`crabfall` sin subcomando, sin bloquear `--help` ni `which crabfall`)
- [x] Inicio: escenarios con su descripción, el estado de la aprobación en el entorno elegido (también si se aprobó con ajustes) y la última corrida; `e` cambia de entorno
- [x] Detalle: el resumen de riesgo de `approve` y un formulario de ajustes con los mismos parsers que los flags; el plan se rearma al aplicar y el error queda en el campo; el comando equivalente
- [x] Check en segundo plano, aprobar tipeando el host (mismo resumen y diff) y correr con `run::execute`, que guarda resultados y reporte como `crabfall run`
- [x] Vista en vivo: fase y avance, indicadores del último segundo, sparklines de p95 y errores, pasos (p50 del último segundo y peor p95 de los últimos 5), avisos; `q` o Ctrl+C frenan con confirmación
- [x] Resultados en pestañas (resumen con comparación contra la anterior, hallazgos, pasos, checks); `o` abre el HTML, `r` repite
- [x] La CLI se reordenó para compartir piezas: `prepare_in`, `approve::{summary, verify, sign}`, `run::execute`
- [x] Tests con `TestBackend`: lista, ajustes válidos e inválidos, correr sin aprobación, host equivocado, check, pantalla de corrida; y dos tests ignorados que imprimen las pantallas (uno con una corrida real)
- Listo: probada por el usuario en `market-api` (20 oct 2026): "es fantástica… buen diseño". Quedan detalles para pulir, a definir con el usuario.
- Después: historial y comparación en la TUI

## Primera versión

**v0.1.0** (19 oct 2026): las rebanadas 0 a 13 con el nombre Crabfall (binario `crabfall`, carpeta `.crabfall/`), en `main` y en GitHub (privado). Lo que queda abierto de esas rebanadas: TLS contra un servidor real (1), la skill en una sesión nueva de Claude Code (2). La rebanada 13 se cerró después, con la corrida de `upload`.

## Backlog (sin orden)

**Al final: modelos de carga nuevos** (`ramping_vus`, iteraciones fijas, arrival por VU). Hoy alcanzan `constant_rate`, `ramping_rate` y `constant_vus`.

Regla de diagnóstico para el ancho de banda (los bytes por segundo se estancan mientras la latencia sube), más reglas de diagnóstico, corrida base fija para CI (`crabfall compare --baseline`), servidor MCP, importadores (OpenAPI, curl, Postman), HTTP/2, GUI con Tauri (después de la TUI), `pick: unique` para datasets, leer `expires_in` para elegir solo cuándo renovar, GitHub Action, correlación con métricas del servidor (OTLP, Prometheus), WebSockets, gRPC, Kafka.

## Decisiones abiertas

| # | Pregunta | Recomendación actual |
| --- | --- | --- |
| D4 | ¿El diagnóstico usa métricas del servidor? | No por ahora: solo lado cliente |

## Registro de decisiones

| Fecha | Decisión | Dónde |
| --- | --- | --- |
| 7 oct 2026 | Claude diseña y analiza; nunca dispara carga | Principio 1 |
| 7 oct 2026 | `hyper` directo en lugar de `reqwest` (confirmado con el spike el 12 oct) | ADR-003 |
| 7 oct 2026 | Diagnóstico en dos capas: reglas en el motor, hipótesis en Claude | Principio 3 |
| 10 oct 2026 | Claude puede hacer smoke checks solo contra loopback | ADR-004 |
| 10 oct 2026 | Formato de escenario YAML declarativo, aprobación derivada | ADR-002 |
| 12 oct 2026 | Claude-first: CLI con JSON + skill y hook de Claude Code (`crabfall claude install`); MCP e importadores después | ADR-001 |
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
| 15 oct 2026 | `crabfall compare`: veredicto por paso con magnitud + test de signos sobre segundos emparejados; regresiones salen con código 1 | ADR-006 |
| 16 oct 2026 | Datasets: `{{dataset.columna}}`, una fila por iteración, CSV como strings y JSON con tipos; se firma el SHA-256 de cada archivo | ADR-007 |
| 17 oct 2026 | Think time después de cada paso; todo ajuste de una corrida entra por flag y se firma | ADR-008 |
| 17 oct 2026 | Reporte HTML autocontenido por corrida, generado por `crabfall run` | ADR-009 |
| 17 oct 2026 | `vu_setup`: cada VU hace su login antes de la carga; el modelo abierto reparte VUs desde un pool | ADR-010 |
| 18 oct 2026 | `vu_refresh`: renovación en segundo plano, sin sacar al VU del pool; si falla, el VU deja de iterar | ADR-011 |
| 18 oct 2026 | `multipart`: archivos leídos una vez y firmados por contenido; sin streaming ni rutas con variables | ADR-012 |
| 19 oct 2026 | Bytes medidos en el socket por request (headers, body y TLS), no en el body; solo la carga | ADR-013 |
| 19 oct 2026 | Nombre: Crabfall; binario `crabfall`, carpeta `.crabfall/`, crates `crabfall-core` y `crabfall-cli` (D2) | — |
| 19 oct 2026 | TUI en la terminal en lugar de la GUI con Tauri, por ahora; los modelos de carga nuevos van al final | Este documento |
| 19 oct 2026 | Primera versión v0.1.0: `dev` se mergea a `main`; repo privado en GitHub | — |
| 19 oct 2026 | Cliente HTTP real por defecto: cookies por VU, redirects hacia `allowed_hosts`, gzip y br; `check` nunca sale de loopback | ADR-014 |
| 19 oct 2026 | TUI: `crabfall` sin argumentos; aprueba con el host tipeado; frena en orden y guarda; el HTML queda para analizar y compartir | ADR-015 |
| 19 oct 2026 | Salida en inglés por defecto y en español a pedido; README y skill en inglés, ADRs y roadmap en español | ADR-016 |
