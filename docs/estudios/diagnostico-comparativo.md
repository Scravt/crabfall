# Estudio — El diagnóstico frente a otras herramientas

24 oct 2026 · para la v0.3 · amplía [ADR-005](../adr/ADR-005-diagnostico.md)

## Preguntas

1. ¿Qué detectan otras herramientas de carga y de observabilidad que Crabfall no detecta?
2. ¿Qué reglas conviene sumar en la v0.3?
3. ¿Las reglas se pueden personalizar o quedan fijas en el motor?

## Qué hace Crabfall hoy

El motor detecta con reglas deterministas sobre lo medido del lado cliente; Claude explica yendo al código y lo marca como hipótesis (principio 3, ADR-005). Mismos datos, mismos hallazgos.

| Regla | Modelo de carga | Qué mira |
| --- | --- | --- |
| `invalid_result` | todos | el generador llegó tarde (lag del scheduler p99 > 5 ms) |
| `breaking_point` | `ramping_rate` | primera ventana sostenida (2 de 3) con descartes, p95 ≥ máx(3× base, base + 50 ms) o errores ≥ 5% |
| `capacity_exceeded` | `constant_rate` | más del 1% de iteraciones descartadas |
| `degradation_over_time` | `constant_rate`, `constant_vus` | el p95 crece en línea recta: final ≥ 2× inicio, +20 ms, R² ≥ 0,6 |
| `error_threshold` | por paso | de < 1% a ≥ 5% de errores, sostenido; distingue error funcional (desde el inicio), sesión vencida (401 a mitad) y rate limit (con la política que declara el servidor) |
| `long_tail` | por paso | p99 ≥ 10× p50 y +50 ms, con ≥ 200 requests |
| `new_connections` | por paso | conexiones nuevas por encima de la concurrencia > 10% de las requests |
| `bandwidth_limit` | todos | el tráfico se estanca en un techo mientras la carga pide un 25% más y el p95 se triplica |

Los umbrales son constantes al principio de `diagnose.rs`. No se pueden cambiar ni apagar.

## Qué hacen las demás

| Herramienta | Enfoque | Qué detecta | ¿Se personaliza? |
| --- | --- | --- | --- |
| **k6** (open source) | Criterios del usuario | Nada por sí solo: `thresholds` y `checks` que escribe el usuario | Todo lo escribe el usuario |
| **Grafana Cloud k6**, Cloud Insights | Reglas fijas con puntaje (0-100, ponderado) | 16 auditorías en tres grupos: confiabilidad (tasa de fallas por URL, Web Vitals), buenas prácticas del script (sin `thresholds`, sin `checks`, demasiados valores de tags, mucho contenido de terceros, versión vieja) y salud del generador (CPU y memoria) | Se puede apagar o elegir auditorías (`cloud.insights.disabled`, `enabledSets`); los umbrales no se cambian. Solo en la nube |
| **k6 Cloud**, Performance Insights (la versión anterior) | Reglas fijas | *Throughput limit*: las requests en curso suben con los VUs y las completadas quedan planas. Fallas en aumento: ~10% por encima del inicio. Fallas altas al principio: > 15% en las primeras 100 iteraciones, "suele ser el script" | No |
| **Gatling** (open source) | Criterios del usuario | `assertions` globales o por request sobre percentiles y porcentaje de fallas | Todo lo escribe el usuario |
| **Gatling Enterprise**, AI Analysis (jun 2026) | IA generativa | Resumen de una corrida, tendencia de las últimas corridas, y comparación de 2 a 5 corridas con veredicto (`Similar`, `SomeDiscrepancies`, `Divergent`), confianza y 2-4 recomendaciones | No; lo habilita un admin. Gatling aclara que hay que verificarlo antes de decidir |
| **NeoLoad**, Augmented Analysis | Aprendizaje automático | Parte la corrida en intervalos (sano, degradado, neutro) con un nombre ("posible caída con 2000 VUs") a partir de latencia, errores, throughput y VUs | No dice que se ajuste; se pueden agregar intervalos a mano |
| **LoadRunner Enterprise** | Analítica (según el fabricante) | Detección de anomalías en vivo y después, correlación, tendencias | No documentado públicamente |
| **Azure Load Testing**, Actionable Insights (preview, may 2025) | IA generativa con métricas del servidor | Picos de latencia, fallas, anomalías de throughput y cuellos de botella de recursos, correlacionando cliente y servidor (por ejemplo, más VUs junto con latencia en Cosmos DB) | Se eligen los componentes y métricas del servidor |
| **Locust** | Criterios del usuario | Sale con 1 si falló alguna request; el resto con un listener propio (fallas > 1%, p95 > 800 ms) | Todo lo escribe el usuario |
| **JMeter**, reporte HTML | Puntaje estándar | APDEX por transacción (satisfecho / tolerado) | Umbrales globales o por transacción |
| **Artillery** | Criterios del usuario y puntaje | Plugin `ensure` (pasa/falla) y `apdex` | Todo lo escribe el usuario |
| **Dynatrace** (APM, producción) | Líneas base que aprenden solas | Degradación respecto de lo normal, con margen absoluto y relativo (ejemplo: 100 ms y 50% sobre la base), persistencia ("3 de 5 muestras") y sin alertar con poco tráfico | Sensibilidad, márgenes, y elegir entre base adaptativa, estacional o umbral fijo |

## Lo que sale de la comparación

### 1. Hay tres enfoques, y Crabfall está en el que se puede explicar

- **Criterios del usuario** (k6, Gatling, Locust, Artillery): dicen si pasó, no qué pasó. Crabfall ya los tiene: `thresholds`, `checks` y `--threshold`.
- **Reglas fijas** (k6 Cloud, Crabfall): mismo dato, mismo hallazgo, con números. Se pueden testear con un laboratorio.
- **ML o IA generativa** (NeoLoad, Gatling Enterprise, Azure, LoadRunner): explican más, pero no son reproducibles ni verificables, y los propios fabricantes piden revisar lo que dicen. Ninguno separa lo medido de lo supuesto.

Crabfall ya cubre lo que da la IA generativa con Claude, y lo marca como hipótesis. **Recomendación: no meter ML ni LLM en el motor.** Las reglas siguen siendo deterministas.

### 2. Lo que Crabfall detecta y las demás no (que se vea en la doc)

- El **punto de quiebre con descartes** en modelo abierto, sin coordinated omission: las herramientas con VUs lo confunden con "latencia alta".
- El **generador como cuello de botella** con el lag del scheduler, no solo por CPU como en k6 Cloud.
- El **límite que declara el servidor** (`RateLimit-Policy`, `Retry-After`), la **sesión vencida** (401 a mitad) y el **error funcional** (fallas desde el inicio, como la regla de k6 de las primeras 100 iteraciones).
- **Conexiones nuevas** de más y **ancho de banda**.

### 3. Lo que falta, por valor

| # | Regla candidata | La tiene | Por qué |
| --- | --- | --- | --- |
| 1 | **Saturación con VUs fijos** (`throughput_plateau`) | k6 (*throughput limit*), NeoLoad | Con `constant_vus` no hay descartes: `capacity_exceeded` nunca se dispara y `breaking_point` solo mira rampas. Por la ley de Little (concurrencia = throughput × latencia), si los VUs están ocupados y el throughput no sube, la latencia es la que paga. Hoy esa corrida sale "sin hallazgos". Con `ramping_vus` (backlog) va a ser igual o peor. |
| 2 | **Servidor o payload** (TTFB contra recepción) | — (estaba en ADR-005) | Ya medimos las fases de cada request. Separa "el servidor tarda en contestar" de "la respuesta es grande o el enlace es lento", y le dice a Claude dónde mirar en el código. |
| 3 | **Contagio entre pasos** como hallazgo propio | NeoLoad (en parte) | Hoy está dentro de `breaking_point`. Que un paso sano se degrade cuando otro se satura apunta a un recurso compartido (pool, CPU, lock). |
| 4 | **Buenas prácticas del escenario** | k6 Cloud | Sin `checks` (un 200 con error no se ve), modelo cerrado sin `think`, demasiadas URLs distintas. Van como avisos de `validate` o de `approve`, no como hallazgos de la corrida. |
| 5 | **Fases de la corrida** | NeoLoad | Un resumen de la corrida por tramos (estable, degradándose, saturada, recuperándose). Ayuda al reporte y a Claude. Es una vista de los hallazgos y la serie más que una regla nueva. |
| 6 | **Tendencia en varias corridas** | Gatling Enterprise | Hoy se compara de a dos o contra la base. Una deriva lenta de versión en versión no se ve. Mejor después de "aprobar en CI". |
| — | Métricas del servidor | Azure, APM | Sigue afuera (D4). Es lo que más suma, pero cambia el alcance: queda para después de la v0.3. |

Las reglas 1 y 2 son las más claras: no tienen dueño en otras herramientas de código abierto, salen de datos que ya medimos y se pueden probar con el laboratorio (un servidor con un pool fijo bajo `constant_vus`, y respuestas grandes por un enlace lento).

## ¿Personalizables?

Lo que muestra la comparación:

- **Los criterios de pasa/falla los define el usuario en todas.** Crabfall ya lo hace con `thresholds`.
- **Las reglas de diagnóstico, nadie las deja reescribir.** k6 Cloud deja apagar auditorías, no cambiar umbrales. Dynatrace, que vive de esto en producción, deja ajustar sensibilidad y márgenes, no escribir reglas.

Opciones:

| | Qué es | A favor | En contra |
| --- | --- | --- | --- |
| A | Fijas, como hoy | Reproducibles; dos proyectos dicen lo mismo con los mismos datos | Un sistema ruidoso da falsos positivos que no se pueden callar |
| B | **Apagar una regla** por escenario o en `config.yaml` | Barato; resuelve el ruido conocido ("este endpoint siempre tiene cola larga") | Se puede apagar algo que importa |
| C | **Ajustar unos pocos umbrales** (sensibilidad o márgenes) | Cubre sistemas muy rápidos o muy lentos (+50 ms no significa lo mismo con 2 ms que con 2 s) | Se ajusta hasta que no salga nada; comparar corridas con umbrales distintos engaña |
| D | Reglas propias (DSL o plugins) | Máxima flexibilidad | Caro; se pisa con `thresholds`; para lo que no cubren las reglas, ya está Claude |

**Recomendación: B y una versión acotada de C; no D.**

- `diagnosis:` en el escenario o en `config.yaml`, y por flag (principio 7): `off: [long_tail]` y unos pocos ajustes con nombre y límites (por ejemplo, `latency_margin` y `min_requests`), no las constantes internas.
- Lo que se cambió queda en `results.json` y en el reporte. Si dos corridas usan ajustes distintos, `compare` avisa, como hoy cuando cambia el escenario.
- No se firma (no cambia el tráfico), igual que los `thresholds`.
- La skill le dice a Claude que no apague ni afloje reglas para que una corrida "pase": si cree que es ruido, se lo propone a la persona.

## Decisiones para tomar

1. ¿Las reglas siguen deterministas, sin ML ni LLM en el motor? (recomendado: sí)
2. ¿Qué reglas entran en la v0.3? (recomendado: 1 y 2; la 3 si alcanza; la 4 como avisos de `validate`)
3. ¿Personalización? (recomendado: B y C acotada; no D)
4. ¿Las fases (5) y la tendencia (6) van ahora o después?

Lo que se decida va a un ADR nuevo (o amplía ADR-005), con su laboratorio.

## Fuentes

- Grafana Cloud k6: [Cloud Insights, referencia](https://grafana.com/docs/grafana-cloud/testing/k6/analyze-results/get-cloud-insights/reference/), [configurar](https://grafana.com/docs/grafana-cloud/testing/k6/analyze-results/get-cloud-insights/configure/), [Performance Insights](https://grafana.com/docs/grafana-cloud/k6/analyze-results/get-performance-insights/), [Cloud Insights GA (jul 2024)](https://grafana.com/whats-new/2024-07-15-cloud-insights-in-grafana-cloud-k6-is-generally-available/)
- Gatling: [assertions](https://docs.gatling.io/concepts/assertions/), [AI Analysis](https://gatling.io/product/ai-analysis), [AI run comparison](https://docs.gatling.io/ai-for-analysis/ai-run-comparison/index.md), [tendencias y comparación](https://gatling.io/updates/ai-analysis-trends-comparison)
- NeoLoad: [Augmented analysis](https://docs.tricentis.com/neoload-web-v2/en-us/content/references/augmented_analysis.htm)
- LoadRunner Enterprise: [resumen de capacidades (NC DIT)](https://it.nc.gov/services/software-quality-security-testing/testing-tools-capabilities/loadrunner-enterprise)
- Azure Load Testing: [actionable insights](https://learn.microsoft.com/ga-ie/azure/app-testing/load-testing/how-to-analyze-test-results-using-actionable-insights), [métricas del servidor](https://learn.microsoft.com/en-us/azure/app-testing/load-testing/how-to-monitor-server-side-metrics)
- Locust: [sin interfaz web, códigos de salida](https://docs.locust.io/en/latest/running-without-web-ui.html)
- JMeter: [reporte HTML y APDEX](https://jmeter.apache.org/usermanual/generating-dashboard.html)
- Artillery: [apdex](https://www.artillery.io/docs/reference/extensions/apdex), [buenas prácticas, ensure](https://artillery.io/docs/get-started/best-practices)
- Dynatrace: [detección de anomalías](https://docs.dynatrace.com/managed/dynatrace-intelligence/anomaly-detection), [ajustar sensibilidad](https://docs.dynatrace.com/docs/dynatrace-intelligence/anomaly-detection/adjust-sensitivity-anomaly-detection)
- Saturación: [punto de saturación de una instancia (OneUptime, 2026)](https://oneuptime.com/blog/post/2026-09-08-single-instance-saturation-point/view), [USL (Baron Schwartz)](https://www.datacouncil.ai/hubfs/DataEngConf/NYC%2018/slides/Quantifying%20Scalability%20With%20the%20Universal%20Scalability%20Law%20-%20Baron%20Schwartz.pdf?hsLang=en)
