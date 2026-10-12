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

Cada rebanada funciona de punta a punta antes de pasar a la siguiente.

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
- Listo: probado contra `shop-api` (API local de ejemplo, repo aparte). **Pendiente:** una API real del trabajo y TLS contra un servidor real.

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
- [ ] `setup`, `extract`, `defaults`, varios flows con `weight`
- [ ] Interpolación con preservación de tipo y placeholders dinámicos
- [ ] Executors `constant_vus` y `ramping_rate`; checks y thresholds
- [ ] Códigos de salida: pasa, thresholds fallidos, error de configuración

**5. Diagnóstico**
- [ ] Series por segundo (latencia, RPS, errores) y desglose de timings
- [ ] Primeras reglas: punto de quiebre, errores por umbral, resultado inválido (generador saturado)
- [ ] `lt findings [--json]` con evidencia y `source` del paso
- [ ] Skill: sección de diagnóstico con hallazgos
- [ ] Validar cada regla con una patología de `lab-server`

## Backlog (sin orden)

Más reglas de diagnóstico, reporte HTML, comparación contra una corrida base, servidor MCP, importadores (OpenAPI, curl, Postman), GUI con Tauri, HTTP/2, datasets, setup por VU, think time, GitHub Action, correlación con métricas del servidor (OTLP, Prometheus), WebSockets, gRPC, Kafka.

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
