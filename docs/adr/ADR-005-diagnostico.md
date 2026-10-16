# ADR-005 — Diagnóstico: hallazgos deterministas + hipótesis del agente

Estado: **aceptado** · 12 oct 2026

## Contexto

Un load tester dice "p99 = 2,2 s". Lo que hace falta es "`login` se satura en ~36/s; los demás pasos se degradan con él; el código está en `src/auth.ts`". El motor ve las métricas pero no el código; Claude ve el código pero no debe inventar métricas.

## Decisión

Dos capas, siempre separadas:

1. **El motor detecta, con reglas deterministas**, sobre lo medido: series por segundo (globales y por paso), métricas por paso y totales. Cada hallazgo trae `id`, severidad, título con números, paso y `source`, ventana de tiempo, `evidence` y causas probables elegidas según la señal (un 429 no sugiere lo mismo que un timeout). Mismos datos, mismos hallazgos.
2. **Claude explica**, yendo de `source` al código, y presenta lo que propone como **hipótesis**, separada de lo medido (skill, sección "Analizar resultados").

### Reglas iniciales (`crates/lt-core/src/diagnose.rs`)

| id | Cuándo | Señal |
| --- | --- | --- |
| `invalid_result` | siempre | lag del scheduler p99 > 5 ms |
| `breaking_point` | `ramping_rate` | primera ventana sostenida (2 de 3) con descartes, p95 ≥ max(3× base, base + 50 ms) o errores ≥ 5% |
| `capacity_exceeded` | tasa constante | > 1% de iteraciones descartadas |
| `degradation_over_time` | tasa constante | recta de mínimos cuadrados del p95: final ≥ 2× inicio, +20 ms, R² ≥ 0,6 |
| `error_threshold` | por paso | pasa de < 1% a ≥ 5% de errores, sostenido; si es desde el inicio, se marca como probable error funcional |
| `long_tail` | por paso, sin saturación | p99 ≥ 10× p50 y +50 ms, con ≥ 200 requests |
| `new_connections` | por paso | conexiones nuevas por encima de `max_vus` (o `vus`) > 10% de las requests: el pool crece hasta la concurrencia, así que una conexión por VU es normal |

La "base" es la mediana del p95 de las primeras ventanas con datos. El paso "culpable" es el que más empeora respecto de su propia base; si varios se degradan juntos, la causa sugerida es un recurso compartido.

### Salida

- `results.json` incluye `series` y `findings`.
- `lt run` imprime los hallazgos al final.
- `lt findings [id] [--json]`: el resumen para agentes (totales, thresholds, hallazgos y métricas por paso, sin series ni cuerpos).

## Validación

`crates/lt-core/tests/diagnose.rs` levanta un servidor con cada patología y verifica que se dispare el hallazgo correcto: pool chico (quiebre en ~200 it/s), rate limiter a 150/s (429 desde ~150 it/s), trabajo que crece con cada request, una de cada 25 requests lenta, `Connection: close` en cada respuesta, y tasa constante por encima de la capacidad. Una rampa sana no dispara nada grave. Un servidor lento con keep-alive no dispara `new_connections` aunque el pool abra decenas de conexiones. Los umbrales están en constantes al principio del módulo para ajustarlos con el laboratorio.

## Consecuencias

- El diagnóstico solo ve el lado cliente. Correlacionar con métricas del servidor (CPU, pool, GC) es la Fase 6 / D4.
- Las reglas tienen umbrales fijos: pueden dar falsos negativos en sistemas muy ruidosos. Se ajustan con casos reales, agregando una patología al laboratorio por cada error del diagnóstico.
- Faltan reglas del catálogo original: "servidor vs payload" (TTFB vs receiving) y contagio entre endpoints como hallazgo propio (hoy aparece dentro de `breaking_point`).

## Prueba con shop-api (15 oct 2026)

- `stress-ramp` (20 → 400 it/s): latencia plana en toda la rampa (p95 ~33 ms, el costo fijo de `create_order`) y sin hallazgos. Correcto: el sistema no se saturó.
- `login-ramp` (10 → 80 it/s): `breaking_point` en ~40 it/s (36 req/s) sobre `login`, con 219 iteraciones descartadas. La causa en el código es `scryptSync` (~28 ms de CPU por llamada que bloquea el event loop: 1000 / 28 ≈ 36/s).
- Esa misma corrida dio un falso positivo de `new_connections`: 200 conexiones nuevas con `max_vus: 200`, todas reutilizadas. Se corrigió la regla y se agregó la patología al laboratorio (`slow_server_with_keep_alive_has_no_new_connections`).
