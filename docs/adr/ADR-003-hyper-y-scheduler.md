# ADR-003 — Cliente HTTP sobre hyper y scheduler de modelo abierto

Estado: **aceptado** · 12 oct 2026 · basado en `spikes/load-spike`

## Contexto

Para diagnosticar hace falta separar dónde se va el tiempo de cada request (DNS, connect, TLS, espera del servidor, descarga), y para que los números sean confiables el generador no puede frenarse cuando el servidor se frena (coordinated omission). `reqwest` no expone los timings por fase.

## Decisión

### 1. Cliente: `hyper` directo, con conexiones propias

El motor abre sus conexiones (`tokio::net::lookup_host` → `TcpStream::connect` → `hyper::client::conn::http1::handshake`) y las reutiliza con un pool propio. Así mide cada fase por separado:

| Fase | Cómo se mide |
| --- | --- |
| DNS | `lookup_host`, solo en conexiones nuevas |
| connect | `TcpStream::connect`, solo en conexiones nuevas |
| TLS | handshake de `tokio-rustls`, solo en conexiones nuevas (**pendiente**: el spike es solo HTTP) |
| TTFB | desde `send_request` hasta recibir los headers |
| receiving | lectura completa del body |

`sending` no se separa de TTFB: con bodies chicos no aporta y hyper no lo expone fácilmente.

### 2. Scheduler: modelo abierto con iteraciones descartadas

- Cada iteración tiene un **instante planificado** (`inicio + i / rate`); el scheduler duerme hasta ese instante y la despacha **sin esperar respuestas**.
- La **latencia se mide desde el instante planificado**, no desde el envío real. También se reporta el tiempo de servicio (desde el envío).
- `max_vus` limita los requests en vuelo. Si no hay un VU libre, la iteración **se descarta y se cuenta**, nunca se demora. Las descartadas son la señal de "el servidor no da abasto".
- Se mide el **lag del scheduler** (despacho real − planificado). Si su p99 supera 5 ms, se avisa que el generador puede ser el cuello de botella.

### 3. Windows: resolución del timer a 1 ms

En Windows, el timer tiene ~15,6 ms de resolución por defecto y todo `sleep` se redondea hacia arriba. Sin corregirlo, el lag p99 del scheduler fue de 27 ms. El motor (y `lab-server`) llaman a `timeBeginPeriod(1)` al iniciar.

## Evidencia (Windows 10, loopback, release)

**10.000 requests a 1.000/s contra `/delay/5`:** la tasa de envío fue 1000.1/s y el lag p99 del scheduler, 2,2 ms.

| ms | p50 | p90 | p95 | p99 | p99.9 |
| --- | --- | --- | --- | --- | --- |
| spike (servicio) | 6.02 | 6.88 | 7.04 | 7.11 | 7.72 |
| spike (desde planificado) | 7.02 | 7.93 | 8.20 | 8.82 | 9.27 |
| oha `--latency-correction` | 6.03 | 7.01 | 7.05 | 7.55 | 8.16 |

**200/s contra `/pool/50` con pool=4 (capacidad: 80/s), `max_vus` 100:** el scheduler mantuvo el plan (lag p99 de 2 ms). Se enviaron 880 requests, con una latencia p50 de 1,27 s, y se descartaron 1.120 iteraciones, con aviso. El throughput real (~79/s) coincide con la capacidad del servidor.

## Consecuencias

- Hay más código propio que con `reqwest`: el pool de conexiones, el manejo de keep-alive y, más adelante, TLS y las redirecciones.
- La latencia "desde planificado" incluye ~1 ms de jitter del scheduler. Si hace falta más precisión, se puede esperar con *spin* el último milisegundo.
- Pendiente: medir TLS con `tokio-rustls` y probar HTTP/1.1 contra servidores reales (cierres de conexión, `Connection: close`).
