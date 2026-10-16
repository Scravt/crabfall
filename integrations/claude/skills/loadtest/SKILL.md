---
name: loadtest
description: Diseña y verifica pruebas de carga con lt (escenarios YAML en .loadtest/) y analiza sus resultados. Usar cuando la persona pide una prueba de carga, de estrés o de performance de una API, o pregunta por resultados en .loadtest/runs/.
---

# Pruebas de carga con lt

Vos diseñás, verificás y analizás. **La persona aprueba y ejecuta la carga.** Nunca corras `lt approve` ni `lt run` (un hook los bloquea, y `lt run` rechaza todo escenario sin una aprobación firmada): cuando el escenario esté listo, pedile que los corra ella.

Podés usar libremente: `lt validate`, `lt check` (solo contra loopback), `lt schema`, `lt findings` y `lt compare`.

## Diseñar un escenario

1. **Entendé la API.** Leé las rutas y los handlers, y el OpenAPI si existe. Anotá para cada endpoint: método, ruta, auth, body, IDs válidos y dónde está el código.
2. **Revisá `.loadtest/config.yaml`** (entornos, `base_url`, `allowed_hosts`, `limits`). Si no existe, crealo con un entorno local:
   ```yaml
   version: 1
   default_environment: local
   environments:
     local:
       base_url: http://127.0.0.1:3000
       allowed_hosts: [127.0.0.1, localhost]
   ```
3. **Escribí `.loadtest/scenarios/<nombre>.yaml`:**
   ```yaml
   version: 1
   name: checkout
   description: 70% navega el catálogo, 30% compra.
   meta: { created_by: agent }
   flows:
     browse:
       weight: 70
       steps:
         - name: list_products
           request: { method: GET, url: /products, query: { page: 1 } }
           source: { file: src/products.ts, symbol: listProducts, operation_id: listProducts }
     buy:
       weight: 30
       steps:
         - name: create_order
           request:
             method: POST
             url: /orders
             headers: { Authorization: Bearer dev-token }
             json: { product_id: 42, quantity: 1 }
           tags: [write]
           source: { file: src/orders.ts, symbol: createOrder }
   load:
     executor: constant_rate
     rate: 50/s          # iteraciones por segundo, no requests
     duration: 1m
     max_vus: 100
   ```
   Reglas:
   - Cada paso tiene un `name` estable y único (`snake_case`). Las métricas se agrupan por ese nombre.
   - Completá siempre `source`: el diagnóstico lo usa para volver al código.
   - Marcá con `tags: [write]` todo paso que cree, modifique o borre datos.
   - Los `weight` reflejan la mezcla de tráfico pedida; son relativos.
   - La tasa de requests es `rate × pasos del flow más largo`. Tiene que quedar dentro de `limits.max_rps`.
   - `max_vus` ≈ `rate × latencia esperada (s) × 5`, con un mínimo de 10. Si es bajo, `lt` descarta iteraciones.
   - Si no conocés un valor, escribí `{ todo: "qué hace falta" }`. No inventes IDs ni credenciales.
   - **Variables:** `vars:` en la raíz y `{{nombre}}` en `url`, `query`, `headers` y bodies. Si un valor JSON es exactamente `"{{x}}"`, conserva el tipo (número, booleano). Dinámicos: `{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt(1,100)}}`. Credenciales: `{{env.NOMBRE}}`, declarando `NOMBRE` en `secrets:` de `config.yaml` (el valor sale del entorno o de `.env`; nunca lo escribas en el escenario). El host de una URL no puede tener variables.
   - **`defaults:`** `headers` y `timeout` comunes a todos los pasos de `flows`.
   - **Encadenar requests:** `setup:` (lista de pasos en la raíz) corre una vez antes de la carga; usalo para el login. `extract:` en un paso saca valores de la respuesta para los pasos siguientes:
     ```yaml
     setup:
       - name: login
         request: { method: POST, url: /auth/login, json: { user: "{{env.LT_USER}}", password: "{{env.LT_PASSWORD}}" } }
         extract:
           token: { json: $.access_token }      # también { header: location } o { regex: "id=(\d+)" }
     defaults:
       headers: { Authorization: "Bearer {{token}}" }
     ```
     Lo extraído en `setup` vale para todos los flows; lo extraído en un flow, solo para los pasos siguientes de esa iteración. Si el `setup` falla, la corrida se aborta; si un `extract` falla, se corta esa iteración.
   - **Checks** (en cada paso, no bloqueantes): `- status: 201`, `- status_in: [200, 201]`, `- { json: $.id, exists: true }`, `- { json: $.status, equals: paid }` (admite `"{{variable}}"`), `- { json: $.name, matches: "^a" }`, `- { header: content-type, contains: json }`, `- body_contains: ok`, `- body_matches: "regex"`. Si un paso tiene checks de status, `lt check` no exige 2xx.
   - **Thresholds** (en la raíz; si alguno falla, `lt run` sale con código 1):
     ```yaml
     thresholds:
       - { metric: http.latency.p95, lt: 300ms }            # p50…p99.9, avg, max
       - { metric: http.latency.p99, step: create_order, lt: 800ms }   # o tag: write
       - { metric: http.error_rate, lt: 1% }                # sin respuesta o status >= 400
       - { metric: checks.pass_rate, gt: 99% }
       - { metric: http.rps, gte: 40 }
     ```
     Checks y thresholds no cambian el tráfico: editarlos no invalida la aprobación.
   - **Executors:**
     - `constant_rate` (`rate`, `duration`, `max_vus`): tasa fija; el default para medir latencia.
     - `ramping_rate` (`start_rate`, `stages: [{ target: 200/s, duration: 2m }, …]`, `max_vus`): la tasa cambia linealmente; usalo para buscar el punto de quiebre.
     - `constant_vus` (`vus`, `duration`): N usuarios en loop; la tasa depende de la latencia. Usalo solo si piden "N usuarios concurrentes"; para medir latencia preferí los de tasa.
4. **Validá:** `lt validate <nombre> --json`. Corregí hasta que `valid` sea `true`. Cada error trae `path`, `line` y `message`.
5. **Verificá:** `lt check <nombre> --json` corre cada flow una vez, en serie. Solo funciona si el entorno apunta a la máquina local; si no, saltealo y decilo. Revisá `status` y `body` de cada paso: un 404 suele ser un ID inexistente, un 401 la auth y un 400 el body.
6. **Entregá** un resumen corto (flows y pesos, tasa, duración, requests/s máximos, pasos que escriben) y los comandos exactos para la persona:
   ```
   lt approve <nombre> --env <entorno>   # muestra el resumen de riesgo y pide tipear el host
   lt run <nombre> --env <entorno>
   ```
   Cualquier cambio posterior al escenario o al entorno invalida la aprobación: si corregís algo, avisá que hay que aprobar de nuevo.

## Analizar resultados

Empezá siempre por `lt findings --json` (la corrida más reciente; `lt findings <id> --json` para otra). Es un resumen compacto: totales, thresholds, hallazgos y métricas por paso. Abrí `.loadtest/runs/<id>/results.json` solo si necesitás la serie por segundo (`series`).

1. **¿El resultado es válido?** Si hay un hallazgo `invalid_result`, decilo primero y no saques conclusiones de esa corrida.
2. **Hallazgos (`findings[]`)**, de más a menos grave. Cada uno trae `title`, `step`, `source`, `window`, `evidence` (los números) y `causes` (causas probables, no conclusiones):
   - `breaking_point`: con `ramping_rate`, desde qué tasa se degrada y qué paso empeora más.
   - `capacity_exceeded`: la tasa pedida supera lo que el sistema sostiene.
   - `error_threshold`: un paso empieza a fallar desde cierta tasa (o desde el inicio: probablemente un error funcional).
   - `degradation_over_time`: con carga constante, la latencia sube sostenidamente.
   - `long_tail`: p99 muy lejos del p50 con carga estable.
   - `new_connections`: no se reutilizan conexiones.
3. **Ir al código.** Para cada hallazgo con `source`, abrí ese archivo y ese símbolo y buscá lo que explicaría la evidencia: trabajo de CPU sincrónico, pools chicos, locks, queries sin índice, colecciones que crecen, llamadas externas sin límite de concurrencia.
4. **Reportá en dos partes, siempre separadas:**
   - **Medido**: lo que dice el hallazgo, con sus números (`evidence`).
   - **Hipótesis**: tu explicación desde el código, con archivo y línea, marcada como hipótesis, y cómo confirmarla (por ejemplo, "si es el pool, subirlo de 5 a 20 debería mover el quiebre de ~170 a ~650 it/s; correlo de nuevo con el mismo escenario").
5. Si no hay hallazgos, mirá los thresholds y los p95/p99 por paso, y decí que no se detectó ningún patrón conocido; no inventes problemas.

## Comparar corridas

Cuando la persona cambió algo (código, configuración, infraestructura) y volvió a correr el mismo escenario, o pregunta "¿mejoró?":

1. `lt compare --json` compara la corrida más reciente con la anterior del mismo escenario y entorno (`lt compare <base> <nueva> --json` para elegirlas).
2. **Leé `warnings` primero.** Si la carga es distinta (`same_load: false`) o el escenario cambió, decilo: las diferencias pueden venir de ahí y no del cambio.
3. **Veredicto por paso (`steps[].change`)**, sobre el p95: `better` y `worse` son cambios grandes y consistentes segundo a segundo (`sign_test`: en cuántos segundos se movió en esa dirección y el p-valor). `uncertain` es un cambio grande pero no consistente: decí que puede ser ruido y sugerí repetir la corrida, no lo presentes como mejora ni como regresión. `same` es un cambio chico.
4. **Reportá** `regressions` e `improvements` con sus números, y los hallazgos `resolved` y `new`. Si se movió un `breaking_point`, ese es el titular ("el quiebre pasó de ~36 a ~140 it/s").
5. Si un hallazgo figura como `resolved` pero la regla de diagnóstico cambió entre versiones de `lt`, aclaralo: no es mérito del cambio.

## Referencia

- `lt schema`: JSON Schema completo del escenario, con descripción de cada campo.
- Unidades: duraciones `500ms`, `30s`, `2m`, `1h`; tasas `50/s`, `3000/m`.
- Códigos de salida: `0` ok · `1` falló (`check` con pasos fallidos, thresholds no cumplidos, `setup` fallido o `compare` con regresiones) · `2` escenario o configuración inválidos · `3` rechazado (host no permitido, límites, no es loopback, sin aprobación válida).
