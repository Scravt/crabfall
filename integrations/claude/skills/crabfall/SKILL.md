---
name: crabfall
description: Diseña y verifica pruebas de carga con Crabfall (escenarios YAML en .crabfall/) y analiza sus resultados. Usar cuando la persona pide una prueba de carga, de estrés o de performance de una API, o pregunta por resultados en .crabfall/runs/.
---

# Pruebas de carga con Crabfall

Vos diseñás, verificás y analizás. **La persona aprueba y ejecuta la carga.** Nunca corras `crabfall approve` ni `crabfall run` (un hook los bloquea, y `crabfall run` rechaza todo escenario sin una aprobación firmada): cuando el escenario esté listo, pedile que los corra ella.

Podés usar libremente: `crabfall validate`, `crabfall check` (solo contra loopback), `crabfall schema`, `crabfall findings` y `crabfall compare`.

## Diseñar un escenario

1. **Entendé la API.** Leé las rutas y los handlers, y el OpenAPI si existe. Anotá para cada endpoint: método, ruta, auth, body, IDs válidos y dónde está el código.
2. **Revisá `.crabfall/config.yaml`** (entornos, `base_url`, `allowed_hosts`, `limits`). Si no existe, crealo con un entorno local:
   ```yaml
   version: 1
   default_environment: local
   environments:
     local:
       base_url: http://127.0.0.1:3000
       allowed_hosts: [127.0.0.1, localhost]
   ```
3. **Escribí `.crabfall/scenarios/<nombre>.yaml`:**
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
   - `max_vus` ≈ `rate × (latencia esperada + pausas de la iteración) (s) × 5`, con un mínimo de 10. Si es bajo, `crabfall` descarta iteraciones.
   - **Pausas** (`think`): si piden simular personas, poné `think: 1s-3s` en `defaults` (o `think` en un paso: `2s` fija, `1s-3s` al azar; `0s` la quita). Va después del paso, no cuenta en la latencia y ocupa el VU. Con `constant_vus` es casi obligatoria: sin pausas, 10 VUs no son 10 personas. No se permite en `setup`, y `crabfall check` la ignora.
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
   - **Login por usuario** (`vu_setup:`, lista de pasos en la raíz): cada VU los corre una vez antes de la carga y lo que extraen queda como variable de ese VU. Es lo correcto para APIs con sesión: con `setup` todos comparten un token, y con el login como primer paso del flow se mide sobre todo el login. Usá `vu_setup` con un dataset de usuarios (`{{users.user}}`): cada VU toma una fila y los flows ven esa misma fila. Se preparan `vus` (o `max_vus`) VUs, uno por login: dimensioná `max_vus` sabiendo que es la cantidad de logins previos. No lleva `think`. Si falla en algunos VUs, quedan afuera; si falla en todos, la corrida se aborta.
   - **Tokens que vencen** (`vu_refresh:` en la raíz, requiere `vu_setup`): si la API emite tokens cortos (mirá el TTL en el código: `expiresIn`, `exp`, `ACCESS_TTL`…) y la corrida dura más que eso, agregá `vu_refresh` con `every` menor que la vida del token menos lo que dura una iteración. Si hay endpoint de refresh, ponelo en `steps` y extraé en `vu_setup` el `refresh_token`; si no, sin `steps` repite el login. Corre en segundo plano y no entra en las métricas. Sin esto, una corrida larga muestra 401 que no son del sistema: si ves 401 que empiezan a mitad de la corrida, sospechá esto antes que del servidor.
   - **Sesión por cookie** (Rails, Django, Laravel, express-session, un BFF): no hace falta `extract` ni un header `cookie`. Cada VU guarda y reenvía sus cookies solo, desde su `vu_setup` hasta su última iteración; poné el login en `vu_setup` y listo. Si la API es por token, las cookies no molestan. `cookies: false` en la raíz solo si el escenario necesita manejarlas a mano.
   - **Redirects:** se siguen solos (hasta 10, solo hacia `allowed_hosts`) y el status es el del final. Si querés verificar el redirect en sí (por ejemplo, que el login responda 302 a `/dashboard`), poné `redirects: 0` en ese request y chequeá `status: 302` y el header `location`. Un redirect a un host fuera de `allowed_hosts` falla con tipo `redirect`: agregá el host al entorno o cortá el redirect.
   - **Formularios** (`application/x-www-form-urlencoded`, típico de logins web y OAuth): `form: { username: "{{users.email}}", password: "{{users.password}}" }` en lugar de `json`. Valores de texto, número o booleano.
   - **Compresión:** cada request pide `gzip, br` y los checks ven el body descomprimido. No agregues `accept-encoding` salvo que quieras medir sin compresión (`identity`).
   - **Subir archivos** (`multipart:` en el `request`, en lugar de `json` o `body`): cada clave es un campo; un string es texto (admite variables) y `{ file: files/foto.jpg, filename: "foto-{{$uuid}}.jpg", content_type: image/jpeg }` es un archivo, relativo a `.crabfall/` (`filename` y `content_type` son opcionales). No pongas el header `content-type`: lo pone Crabfall con el boundary. Usá archivos de prueba del tamaño realista para la API (mirá el límite en el código: `limits.fileSize`, `MAX_UPLOAD`…) y guardalos en `.crabfall/files/`; si no hay, pedíselos a la persona o generá uno sin datos reales. Si la API devuelve un hash o el tamaño, chequealo. Si sube y no borra, el disco del servidor crece con cada iteración: agregá el paso que borra si existe, o avisalo.
   - **Datos por iteración** (usuarios, IDs, búsquedas distintas): `data:` en la raíz, con archivos relativos a `.crabfall/`. Cada iteración toma una fila de cada dataset que usa su flow, y la ve como `{{dataset.columna}}` en todos sus pasos:
     ```yaml
     data:
       users: { file: data/users.csv }                     # CSV con encabezado: todo string
       products: { file: data/products.json, pick: random } # arreglo de objetos: conserva tipos
     ```
     `pick: sequential` (por defecto) recorre las filas en orden y vuelve a empezar; `random` elige una al azar. Las columnas no se pueden usar en `setup`, sí en `vu_setup` (una fila por VU) y en los flows. No inventes usuarios ni credenciales: si no existen datos de prueba, dejá un `todo` y pedíselos a la persona. Editar el archivo invalida la aprobación.
   - **Checks** (en cada paso, no bloqueantes): `- status: 201`, `- status_in: [200, 201]`, `- { json: $.id, exists: true }`, `- { json: $.status, equals: paid }` (admite `"{{variable}}"`), `- { json: $.name, matches: "^a" }`, `- { header: content-type, contains: json }`, `- body_contains: ok`, `- body_matches: "regex"`. Si un paso tiene checks de status, `crabfall check` no exige 2xx.
   - **Thresholds** (en la raíz; si alguno falla, `crabfall run` sale con código 1):
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
4. **Validá:** `crabfall validate <nombre> --json`. Corregí hasta que `valid` sea `true`. Cada error trae `path`, `line` y `message`.
5. **Verificá:** `crabfall check <nombre> --json` corre cada flow una vez, en serie. Solo funciona si el entorno apunta a la máquina local; si no, saltealo y decilo. Revisá `status` y `body` de cada paso: un 404 suele ser un ID inexistente, un 401 la auth y un 400 el body.
6. **Entregá** un resumen corto (flows y pesos, tasa, duración, requests/s máximos, pasos que escriben) y los comandos exactos para la persona:
   ```
   crabfall approve <nombre> --env <entorno>   # muestra el resumen de riesgo y pide tipear el host
   crabfall run <nombre> --env <entorno>
   ```
   Cualquier cambio posterior al escenario o al entorno invalida la aprobación: si corregís algo, avisá que hay que aprobar de nuevo.

   **Ajustes por flag.** Para variar una corrida sin tocar el YAML ("lo mismo con el doble de tasa", "probalo con 20 usuarios", "con otros usuarios"), usá flags en lugar de editar el escenario: `--rate`, `--duration`, `--vus`, `--max-vus`, `--start-rate`, `--stages 100/s:1m,0/s:30s`, `--graceful-stop`, `--executor`, `--think 1s-3s`, `--refresh-every 4m`, `--data users=data/otros.csv`, `--pick users=random` y `--threshold "http.latency.p95<300ms"` (lista en `crabfall run --help`; `--threshold` reemplaza a los del YAML y no requiere aprobar de nuevo). Validalos con `crabfall validate <nombre> <flags> --json`: el error dice qué flag falta o sobra. Los mismos flags van en `crabfall approve` y en `crabfall run`, porque la aprobación los firma. Entregá los dos comandos con los flags completos. Si un ajuste se va a usar siempre, va en el YAML.

## Analizar resultados

Empezá siempre por `crabfall findings --json` (la corrida más reciente; `crabfall findings <id> --json` para otra). Es un resumen compacto: totales, thresholds, hallazgos y métricas por paso. Abrí `.crabfall/runs/<id>/results.json` solo si necesitás la serie por segundo (`series`). Para la persona hay un reporte gráfico en `.crabfall/runs/<id>/report.html` (lo genera `crabfall run`; `crabfall report <id>` lo regenera): mencionáselo al cerrar el análisis. Vos leé el JSON, no el HTML.

1. **¿El resultado es válido?** Si hay un hallazgo `invalid_result`, decilo primero y no saques conclusiones de esa corrida.
2. **Hallazgos (`findings[]`)**, de más a menos grave. Cada uno trae `title`, `step`, `source`, `window`, `evidence` (los números) y `causes` (causas probables, no conclusiones):
   - `breaking_point`: con `ramping_rate`, desde qué tasa se degrada y qué paso empeora más.
   - `capacity_exceeded`: la tasa pedida supera lo que el sistema sostiene.
   - `error_threshold`: un paso empieza a fallar desde cierta tasa (o desde el inicio: probablemente un error funcional). Con 429, si el servidor declaró su límite, la causa y `evidence.rate_limit` traen la cuota, la ventana y el `Retry-After`: buscá en el código dónde se configura ese número y si es por IP, por usuario o por token.
   - `degradation_over_time`: con carga constante, la latencia sube sostenidamente.
   - `long_tail`: p99 muy lejos del p50 con carga estable.
   - `new_connections`: no se reutilizan conexiones.
3. **Ir al código.** Para cada hallazgo con `source`, abrí ese archivo y ese símbolo y buscá lo que explicaría la evidencia: trabajo de CPU sincrónico, pools chicos, locks, queries sin índice, colecciones que crecen, llamadas externas sin límite de concurrencia.
4. **Reportá en dos partes, siempre separadas:**
   - **Medido**: lo que dice el hallazgo, con sus números (`evidence`).
   - **Hipótesis**: tu explicación desde el código, con archivo y línea, marcada como hipótesis, y cómo confirmarla (por ejemplo, "si es el pool, subirlo de 5 a 20 debería mover el quiebre de ~170 a ~650 it/s; correlo de nuevo con el mismo escenario").
5. Si no hay hallazgos, mirá los thresholds y los p95/p99 por paso, y decí que no se detectó ningún patrón conocido; no inventes problemas.

**Bytes.** Cada paso trae `bytes_sent` y `bytes_received` (bytes del socket: headers, body y TLS), y `totals` trae `send_rate` y `receive_rate` en bytes por segundo; la serie los tiene por segundo. Con archivos o respuestas grandes, si la latencia sube mientras los bytes por segundo dejan de crecer aunque la carga sí crezca, el límite puede ser el ancho de banda y no el servidor: decilo como hipótesis (multiplicá por 8 para comparar con un enlace en Mbit/s) y sugerí confirmarlo corriendo el generador más cerca del servidor o con archivos más chicos.

## Comparar corridas

Cuando la persona cambió algo (código, configuración, infraestructura) y volvió a correr el mismo escenario, o pregunta "¿mejoró?":

1. `crabfall compare --json` compara la corrida más reciente con la anterior del mismo escenario y entorno (`crabfall compare <base> <nueva> --json` para elegirlas).
2. **Leé `warnings` primero.** Si la carga es distinta (`same_load: false`) o el escenario cambió, decilo: las diferencias pueden venir de ahí y no del cambio.
3. **Veredicto por paso (`steps[].change`)**, sobre el p95: `better` y `worse` son cambios grandes y consistentes segundo a segundo (`sign_test`: en cuántos segundos se movió en esa dirección y el p-valor). `uncertain` es un cambio grande pero no consistente: decí que puede ser ruido y sugerí repetir la corrida, no lo presentes como mejora ni como regresión. `same` es un cambio chico.
   Si `warnings` dice que la respuesta o el request de un paso cambió de tamaño, mencionalo al lado del cambio de latencia de ese paso: puede ser la explicación (más datos, otra paginación, un archivo más grande) y no un servidor más lento.
4. **Reportá** `regressions` e `improvements` con sus números, y los hallazgos `resolved` y `new`. Si se movió un `breaking_point`, ese es el titular ("el quiebre pasó de ~36 a ~140 it/s").
5. Si un hallazgo figura como `resolved` pero la regla de diagnóstico cambió entre versiones de `crabfall`, aclaralo: no es mérito del cambio.

## Referencia

- `crabfall schema`: JSON Schema completo del escenario, con descripción de cada campo.
- Unidades: duraciones `500ms`, `30s`, `2m`, `1h`; tasas `50/s`, `3000/m`.
- Códigos de salida: `0` ok · `1` falló (`check` con pasos fallidos, thresholds no cumplidos, `setup` fallido o `compare` con regresiones) · `2` escenario o configuración inválidos · `3` rechazado (host no permitido, límites, no es loopback, sin aprobación válida).
