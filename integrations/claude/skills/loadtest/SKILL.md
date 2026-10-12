---
name: loadtest
description: Diseña y verifica pruebas de carga con lt (escenarios YAML en .loadtest/) y analiza sus resultados. Usar cuando la persona pide una prueba de carga, de estrés o de performance de una API, o pregunta por resultados en .loadtest/runs/.
---

# Pruebas de carga con lt

Vos diseñás, verificás y analizás. **La persona aprueba y ejecuta la carga.** Nunca corras `lt approve` ni `lt run` (un hook los bloquea, y `lt run` rechaza todo escenario sin una aprobación firmada): cuando el escenario esté listo, pedile que los corra ella.

Podés usar libremente: `lt validate`, `lt check` (solo contra loopback), `lt schema`.

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
   - **Todavía no soportado:** `setup`, `extract` (encadenar login → token), variables `{{...}}`, checks, thresholds y otros executors. Si el flujo los necesita, usá valores fijos (un token de desarrollo, un ID existente) y avisale a la persona.
4. **Validá:** `lt validate <nombre> --json`. Corregí hasta que `valid` sea `true`. Cada error trae `path`, `line` y `message`.
5. **Verificá:** `lt check <nombre> --json` corre cada flow una vez, en serie. Solo funciona si el entorno apunta a la máquina local; si no, saltealo y decilo. Revisá `status` y `body` de cada paso: un 404 suele ser un ID inexistente, un 401 la auth y un 400 el body.
6. **Entregá** un resumen corto (flows y pesos, tasa, duración, requests/s máximos, pasos que escriben) y los comandos exactos para la persona:
   ```
   lt approve <nombre> --env <entorno>   # muestra el resumen de riesgo y pide tipear el host
   lt run <nombre> --env <entorno>
   ```
   Cualquier cambio posterior al escenario o al entorno invalida la aprobación: si corregís algo, avisá que hay que aprobar de nuevo.

## Analizar resultados

Los resultados están en `.loadtest/runs/<id>/results.json`; el id más alto es el más reciente.

1. **Primero, si el resultado es válido.** Mirá `warnings`, `totals.dropped` y `scheduler_lag_ms`:
   - Iteraciones descartadas: el servidor no dio abasto a esa tasa (o `max_vus` era bajo). Es un hallazgo en sí, y las latencias medidas subestiman la real.
   - Lag del scheduler alto (p99 > 5 ms): el generador fue el cuello de botella. No saques conclusiones.
2. **Después, por paso** (`steps[]`): `latency_ms` (desde el instante planificado), `ttfb_ms`, `connect_ms`, `status_codes`, `failures` y `error_samples`. Compará p50 con p99 y los pasos entre sí.
3. **Por último, el código.** Usá `source` para ir al handler y proponé causas. Separá siempre lo **medido** (con números del resultado) de lo que es **hipótesis**.

## Referencia

- `lt schema`: JSON Schema completo del escenario, con descripción de cada campo.
- Unidades: duraciones `500ms`, `30s`, `2m`, `1h`; tasas `50/s`, `3000/m`.
- Códigos de salida: `0` ok · `1` `check` con pasos fallidos · `2` escenario o configuración inválidos · `3` rechazado (host no permitido, límites, no es loopback, sin aprobación válida).
