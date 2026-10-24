# ADR-005 — Diagnóstico: hallazgos deterministas + hipótesis del agente

Estado: **aceptado** · 12 oct 2026

## Contexto

Un load tester dice "p99 = 2,2 s". Lo que hace falta es "`login` se satura en ~36/s; los demás pasos se degradan con él; el código está en `src/auth.ts`". El motor ve las métricas pero no el código; Claude ve el código pero no debe inventar métricas.

## Decisión

Dos capas, siempre separadas:

1. **El motor detecta, con reglas deterministas**, sobre lo medido: series por segundo (globales y por paso), métricas por paso y totales. Cada hallazgo trae `id`, severidad, título con números, paso y `source`, ventana de tiempo, `evidence` y causas probables elegidas según la señal (un 429 no sugiere lo mismo que un timeout). Mismos datos, mismos hallazgos.
2. **Claude explica**, yendo de `source` al código, y presenta lo que propone como **hipótesis**, separada de lo medido (skill, sección "Analizar resultados").

### Reglas iniciales (`crates/crabfall-core/src/diagnose.rs`)

| id | Cuándo | Señal |
| --- | --- | --- |
| `invalid_result` | siempre | lag del scheduler p99 > 5 ms |
| `breaking_point` | `ramping_rate` | primera ventana sostenida (2 de 3) con descartes, p95 ≥ max(3× base, base + 50 ms) o errores ≥ 5% |
| `capacity_exceeded` | tasa constante | > 1% de iteraciones descartadas |
| `degradation_over_time` | tasa constante | recta de mínimos cuadrados del p95: final ≥ 2× inicio, +20 ms, R² ≥ 0,6 |
| `error_threshold` | por paso | pasa de < 1% a ≥ 5% de errores, sostenido. La ventana arranca en el primer segundo que ya fallaba, aunque tuviera pocas requests. Si es desde el inicio, se marca como probable error funcional; si son 401 a mitad de corrida, como sesión vencida, con la salida según cómo se obtiene el token (ADR-011) |
| `long_tail` | por paso, sin saturación | p99 ≥ 10× p50 y +50 ms, con ≥ 200 requests |
| `new_connections` | por paso | conexiones nuevas por encima de `max_vus` (o `vus`) > 10% de las requests: el pool crece hasta la concurrencia, así que una conexión por VU es normal |

La "base" es la mediana del p95 de las primeras ventanas con datos. El paso "culpable" es el que más empeora respecto de su propia base; si varios se degradan juntos, la causa sugerida es un recurso compartido.

### Salida

- `results.json` incluye `series` y `findings`.
- `crabfall run` imprime los hallazgos al final.
- `crabfall findings [id] [--json]`: el resumen para agentes (totales, thresholds, hallazgos y métricas por paso, sin series ni cuerpos).

## Validación

`crates/crabfall-core/tests/diagnose.rs` levanta un servidor con cada patología y verifica que se dispare el hallazgo correcto: pool chico (quiebre en ~200 it/s), rate limiter a 150/s (429 desde ~150 it/s), trabajo que crece con cada request, una de cada 25 requests lenta, `Connection: close` en cada respuesta, y tasa constante por encima de la capacidad. Una rampa sana no dispara nada grave. Un servidor lento con keep-alive no dispara `new_connections` aunque el pool abra decenas de conexiones. Los umbrales están en constantes al principio del módulo para ajustarlos con el laboratorio.

## Consecuencias

- El diagnóstico solo ve el lado cliente. Correlacionar con métricas del servidor (CPU, pool, GC) es la Fase 6 / D4.
- Las reglas tienen umbrales fijos: pueden dar falsos negativos en sistemas muy ruidosos. Se ajustan con casos reales, agregando una patología al laboratorio por cada error del diagnóstico.
- Faltan reglas del catálogo original: "servidor vs payload" (TTFB vs receiving) y contagio entre endpoints como hallazgo propio (hoy aparece dentro de `breaking_point`).

## Prueba con shop-api (15 oct 2026)

- `stress-ramp` (20 → 400 it/s): latencia plana en toda la rampa (p95 ~33 ms, el costo fijo de `create_order`) y sin hallazgos. Correcto: el sistema no se saturó.
- `login-ramp` (10 → 80 it/s): `breaking_point` en ~40 it/s (36 req/s) sobre `login`, con 219 iteraciones descartadas. La causa en el código es `scryptSync` (~28 ms de CPU por llamada que bloquea el event loop: 1000 / 28 ≈ 36/s).
- Esa misma corrida dio un falso positivo de `new_connections`: 200 conexiones nuevas con `max_vus: 200`, todas reutilizadas. Se corrigió la regla y se agregó la patología al laboratorio (`slow_server_with_keep_alive_has_no_new_connections`).

## El límite que declara el servidor (19 oct 2026)

Un 429 casi siempre viene con información: `Retry-After` y, cada vez más, la política del límite (`RateLimit-Policy` de los drafts de la IETF, como manda express-rate-limit; o el `X-RateLimit-Limit` de siempre). El motor guarda eso de cada 429 en `steps[].rate_limit` (cantidad, `Retry-After` mínimo y máximo, cuota y ventana). Cuando `error_threshold` encuentra que el 429 domina, la causa deja de ser genérica y dice el límite con números ("declara 30 requests cada 60 s (`RateLimit-Policy`) y pide esperar con `Retry-After 23 s`"), y la evidencia lo incluye. `crabfall run` lo muestra en una línea `límite`. Solo se lee la forma numérica de `Retry-After`; una fecha HTTP se ignora.

## Ancho de banda (20 oct 2026)

`bandwidth_limit` (media): el tráfico (bytes enviados + recibidos por segundo, ADR-013) llega a un techo y se queda ahí mientras la carga pide bastante más y el p95 se dispara.

- **Cuándo mira:** solo si el pico pasa 1 MB/s; con menos, el ancho de banda casi nunca es el límite.
- **El techo:** el primer segundo con al menos el 85% del pico; desde ahí, 5 segundos o más, y el 70% de ellos, en ese nivel.
- **Lo que pedía la carga:** las iteraciones ofrecidas (iniciadas + descartadas) después del techo, por los bytes por iteración de antes. Tiene que superar en un 25% lo que se movió.
- **La latencia:** el p95 mediano después del techo, al menos 3 veces (y 50 ms más que) el de antes, como en `breaking_point`.
- **Evidencia:** pico, bytes/s movidos y pedidos, bytes por iteración y los p95. Las causas empiezan por el enlace (Wi-Fi, VPN, placa, plan), siguen con un proxy o CDN que limita, y terminan en respuestas grandes.
- **Laboratorio:** respuestas de 100 KB por un "enlace" simulado de 2 MB/s, bajo una rampa de 5 a 60 it/s (hasta 6 MB/s pedidos), dan el hallazgo; las mismas respuestas sin el enlace, no.
