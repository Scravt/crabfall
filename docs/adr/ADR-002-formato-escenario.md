# ADR-002 — Formato del escenario

Estado: **aceptado** · 10 oct 2026 · revisado 12 oct 2026 (ADR-001: Claude-first)

## Contexto

El escenario es el contrato central de la herramienta. Lo escribe Claude (ADR-001) o una persona a mano; más adelante también los importadores. Lo firma la aprobación, lo ejecuta el motor y el diagnóstico cuelga sus hallazgos de él. Además vive versionado en Git, en los repos de los usuarios. Cambiarlo después de la v0.1 rompe a todos, así que se decide ahora.

Requisitos, en orden de prioridad:

1. **Analizable estáticamente.** Antes de ejecutar hay que poder calcular hosts destino, tasa máxima de requests y duración, para el resumen de aprobación y los límites duros.
2. **Fácil de escribir bien para un LLM.** Validado por JSON Schema, con errores que señalan el campo exacto y sin campos desconocidos que se ignoren en silencio.
3. **Legible y editable por humanos**, con diffs limpios en Git.
4. **Destino natural de futuros importadores**, sin pérdida de lo que importa.

## Decisión

### 1. Datos declarativos en YAML, no código

El escenario es un documento YAML (también se acepta JSON) validado contra un JSON Schema generado desde los tipos Rust con `schemars`.

**Alternativas descartadas:**

- **Scripts en JavaScript, como k6:** no se pueden analizar sin ejecutarlos, así que no hay resumen de aprobación confiable. Además, un agente que escribe código que la herramienta ejecuta rompe el principio 1.
- **Formato propio, como `.bru` de Bruno:** obliga a escribir parser, resaltado y validación propios, y los LLM no lo conocen.
- **TOML:** el anidamiento profundo (flows → steps → request → body) se vuelve ilegible.

El costo de lo declarativo es que no hay lógica arbitraria (ifs, loops, cálculos). Lo cubren `extract`, la interpolación y los placeholders dinámicos. Si un caso real lo exige, se evalúa un mini-lenguaje de expresiones; nunca código arbitrario.

### 2. La aprobación no vive en el archivo

El escenario **no tiene un campo `status`**. Un escenario está aprobado si y solo si existe en `.loadtest/approvals/` una firma válida para su hash actual. Cualquier edición cambia el hash y el escenario vuelve a ser borrador automáticamente, sin que nadie tenga que acordarse de marcarlo.

**Qué se firma** (detalle en ADR-004): el hash del escenario **junto con el entorno resuelto** contra el que va a correr (base URL, hosts permitidos, variables y límites). Si no se incluye el entorno, un agente podría dejar intacto un escenario aprobado y cambiar la `base_url` de `local` para que apunte a producción.

**Cómo se calcula el hash:** se parsea el YAML al modelo tipado, se serializa a JSON canónico (claves ordenadas y valores por defecto completos) y se aplica SHA-256. Los comentarios de YAML desaparecen al parsear. **Se excluyen `description` y `meta`**, que no cambian el tráfico. Si una versión nueva de la herramienta cambia un valor por defecto, las aprobaciones se invalidan, y eso es correcto: el tráfico cambió.

### 3. Estructura: setup, flows con peso y load

- **`setup`**: pasos que corren **una vez por ejecución**, antes de la carga (por ejemplo, login). Sus `extract` quedan como variables globales de solo lectura.
- **`flows`**: cada iteración elige un flow según su `weight` y ejecuta sus `steps` en orden. Así se modela una mezcla de tráfico realista (70% navega, 30% compra) en un solo escenario. `weight` es opcional y vale `1` por defecto; los pesos son relativos, no tienen que sumar 100.
- **`defaults`**: headers y `timeout` que heredan **solo los pasos de `flows`**. `setup` no hereda nada, porque suele correr antes de que existan las variables que `defaults` usa (por ejemplo, `{{token}}`).
- **`load`**: un único executor por escenario. La tasa se expresa en **iteraciones** por segundo, no en requests.

Como un flow puede tener varios pasos, la tasa de requests no es igual a la de iteraciones. El resumen de aprobación muestra la tasa máxima de requests calculada como `rate × máximo de steps entre los flows`, que es la cota segura. **Los límites duros (`limits.max_rps`) se miden en requests por segundo** y se comparan contra esa cota.

### 4. Cada paso tiene un nombre estable, y ese nombre es la clave de las métricas

`name` es obligatorio y único dentro del escenario (`[A-Za-z0-9_.-]+`, compatible con los `operationId` de OpenAPI). Las métricas, los thresholds y los hallazgos se agrupan por `name`, **nunca por la URL interpolada**: `/orders/123` y `/orders/456` son el mismo endpoint. Así se evita la explosión de series y el diagnóstico puede hablar de "`create_order`".

### 5. `source` conecta el paso con el código

Es un campo opcional y libre: `file`, `symbol` y `operation_id`. El motor no lo usa; solo lo copia en los hallazgos. Lo completa Claude cuando diseña desde el código (o desde un OpenAPI, con `operation_id`). Es lo que permite que `/loadtest:diagnose` vaya del hallazgo directo al handler responsable.

### 6. Interpolación y tipos

- `{{var}}`: variables del escenario, del entorno, de `--var` en la CLI o de un `extract`. La prioridad es, de menor a mayor: escenario → entorno → CLI → extract.
- `{{env.NOMBRE}}`: variables de entorno del sistema o de `.env`. Solo se permiten las declaradas en `secrets` de `config.yaml`. En toda salida (terminal, JSON, reportes) se muestran redactadas.
- `{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt(1,100)}}`: valores dinámicos, uno nuevo por uso.
- **Preservación de tipo:** si un valor JSON es *exactamente* `"{{var}}"`, conserva el tipo de la variable (número, booleano, objeto). Si está mezclado con texto, el resultado es un string. Esto evita mandar `"42"` donde la API espera `42`.

### 7. Estricto con lo desconocido

Un campo desconocido es un **error**, no un aviso, porque atrapa errores de tipeo de humanos y alucinaciones de LLM. Para extensiones de terceros se permiten campos con prefijo `x-`, que el motor ignora.

### 8. `todo` explícito para lo que falta

Claude (y los futuros importadores) marcan los valores que no conocen con `{ todo: "qué hace falta" }`. `lt validate` falla mientras quede alguno y lista cada uno con su ruta. Así se ve exactamente qué tiene que completar la persona, en lugar de que la prueba falle con un 404 en plena carga.

### 9. Versionado del formato

`version: 1` es obligatorio. Agregar campos opcionales no sube la versión; un cambio incompatible sí, y viene con `lt migrate`. La herramienta rechaza versiones que no conoce, con un mensaje que dice qué versión de `lt` hace falta.

### 10. Parser: `serde-saphyr` + `serde_path_to_error`

Evaluado el 12 oct 2026 contra `serde_yaml_ng` y `serde_norway` (forks de `serde_yaml`, que está archivado; `serde_yml` está deprecado). Es el único que da línea y columna en todos los casos probados:

| Caso | forks de serde_yaml | serde-saphyr + path_to_error |
| --- | --- | --- |
| campo mal escrito | ruta + línea | ruta + línea |
| tipo incorrecto | ruta + línea | ruta + línea |
| variante desconocida (`executor`) | ruta + línea | ruta + línea |
| campo desconocido dentro de un enum etiquetado | **nada** | ruta + línea del bloque |
| clave duplicada | **sin línea** | línea |

Además está escrito en Rust puro, se mantiene activamente y ofrece `Spanned<T>`, que permite ubicar errores semánticos (por ejemplo, un `name` de paso repetido). `validate --json` reporta `path`, `line`, `column` y `message`.

## Ejemplos

### Configuración del proyecto: `.loadtest/config.yaml`

```yaml
version: 1
default_environment: local

environments:
  local:
    base_url: http://localhost:3000
    allowed_hosts: [localhost, 127.0.0.1]
  staging:
    base_url: https://staging.api.ejemplo.com
    allowed_hosts: [staging.api.ejemplo.com]
    vars:
      product_id: 1001
    limits:                 # límites duros: ni una aprobación los supera
      max_rps: 200          # requests por segundo, no iteraciones
      max_vus: 300
      max_duration: 15m

secrets:                    # solo nombres; los valores vienen del entorno o de .env (gitignored)
  - LT_USER
  - LT_PASSWORD
```

### Escenario escrito a mano o por el agente: `.loadtest/scenarios/checkout.yaml`

```yaml
version: 1
name: checkout
description: Usuarios que navegan el catálogo; algunos compran.

meta:
  created_by: agent         # agent | human | import:openapi | import:curl ...

vars:
  product_id: 42

setup:
  - name: login
    request:
      method: POST
      url: /auth/login
      json:
        user: "{{env.LT_USER}}"
        password: "{{env.LT_PASSWORD}}"
    checks:
      - status: 200
    extract:
      token: { json: $.access_token }

defaults:                   # se aplica a todos los pasos de los flows
  headers:
    Authorization: "Bearer {{token}}"
  timeout: 5s

flows:
  browse:
    weight: 70
    steps:
      - name: list_products
        request:
          method: GET
          url: /products
          query:
            page: "{{$randomInt(1,20)}}"
        checks:
          - status: 200
        source: { file: src/products/routes.ts, symbol: listProducts }

  buy:
    weight: 30
    steps:
      - name: create_order
        request:
          method: POST
          url: /orders
          json:
            product_id: "{{product_id}}"     # se envía como número
            quantity: 1
            idempotency_key: "{{$uuid}}"
        checks:
          - status: 201
          - json: $.id
            exists: true
        extract:
          order_id: { json: $.id }
        tags: [write]
        source: { file: src/orders/handler.ts, symbol: createOrder }

      - name: get_order
        request:
          method: GET
          url: /orders/{{order_id}}
        checks:
          - status: 200
          - header: content-type
            contains: application/json

load:
  executor: ramping_rate
  start_rate: 10/s
  stages:
    - { target: 100/s, duration: 2m }
    - { target: 100/s, duration: 5m }
    - { target: 0/s, duration: 30s }
  max_vus: 200
  graceful_stop: 30s

thresholds:
  - metric: http.latency.p95
    lt: 300ms
  - metric: http.latency.p99
    step: create_order
    lt: 800ms
  - metric: http.error_rate
    lt: 1%
  - metric: checks.pass_rate
    gt: 99%
```

Resumen de aprobación que se calcula de este archivo con `--env staging`:

```
Escenario   checkout  (cambió desde la última aprobación: +1 paso en flow "buy")
Entorno     staging → staging.api.ejemplo.com
Carga       ramping_rate hasta 100 it/s ≈ 200 req/s máx. (2 pasos por iteración)
Duración    7m30s + 30s graceful stop
Límites     dentro de max_rps 200 ✓  max_vus 300 ✓  max_duration 15m ✓
Escribe     sí: create_order (POST /orders), marcado [write]
```

### Escenario generado por Claude desde un `openapi.yaml`

```yaml
version: 1
name: smoke
description: Generado desde openapi.yaml — 12 operaciones incluidas, 2 omitidas (ver meta).

meta:
  created_by: agent
  generated_from: openapi.yaml
  skipped:
    - { operation_id: uploadImage, reason: "multipart/form-data todavía no soportado" }
    - { operation_id: streamEvents, reason: "text/event-stream no soportado" }

vars:
  petId: { todo: "ID de un pet existente en el entorno" }

flows:
  main:
    steps:
      - name: listPets
        request:
          method: GET
          url: /pets
          query: { limit: 10 }
        checks: [{ status: 200 }]
        source: { operation_id: listPets }

      - name: createPet
        request:
          method: POST
          url: /pets
          json: { name: "string", tag: "string" }   # ejemplo derivado del schema
        checks: [{ status: 201 }]
        source: { operation_id: createPet }

      - name: showPetById
        request:
          method: GET
          url: /pets/{{petId}}
        checks: [{ status: 200 }]
        source: { operation_id: showPetById }

load:
  executor: constant_vus
  vus: 1
  duration: 30s
```

`lt validate` responde:

```
✗ vars.petId: valor pendiente (todo) — "ID de un pet existente en el entorno"
1 pendiente. Completalo a mano, pasalo con --var petId=..., o pedile al agente que lo resuelva.
```

## Referencia rápida del modelo (v1)

| Bloque | Campos | Notas |
| --- | --- | --- |
| raíz | `version`, `name`, `description`, `meta`, `vars`, `setup`, `defaults`, `flows`, `load`, `thresholds` | Obligatorios: `version`, `name`, `flows`, `load` |
| flow | `weight`, `steps` | `weight` opcional, por defecto `1` |
| defaults | `headers`, `timeout` | Solo para pasos de `flows`, no de `setup` |
| paso | `name`, `request`, `checks`, `extract`, `tags`, `source` | `name` único en todo el escenario |
| request | `method`, `url`, `headers`, `query`, `timeout` + uno de `json` / `form` / `body` | `url` relativa a `base_url` o absoluta; los hosts se validan contra `allowed_hosts` |
| check | `status`, `status_in`, `json` + (`exists` / `equals` / `matches`), `header` + (`equals` / `contains` / `matches`), `body` + `matches` | No bloqueantes: cuentan para `checks.pass_rate` |
| extract | `{ json: <path> }`, `{ header: <nombre> }`, `{ regex: <patrón> }` | En `setup`: global de solo lectura. En flows: por iteración |
| load | `constant_vus` (`vus`, `duration`), `constant_rate` (`rate`, `duration`, `max_vus`), `ramping_rate` (`start_rate`, `stages`, `max_vus`), `breakpoint` (futuro) | Todos aceptan `graceful_stop` |
| threshold | `metric`, `step` o `tag` opcional, uno de `lt` / `lte` / `gt` / `gte` | Métricas: `http.latency.pXX`, `http.error_rate`, `http.rps`, `checks.pass_rate` |

Unidades: duraciones `500ms`, `30s`, `2m`; tasas `50/s`, `3000/m`; porcentajes `1%`.

## Smoke check

Cualquier escenario se puede verificar con `lt check <escenario>`. Corre el `setup` y cada flow **una sola vez**, en serie y con 1 VU, e **ignora el bloque `load`**. Por eso no es un executor: es un modo de ejecución. Las reglas que lo hacen seguro para un agente están en ADR-004.

## Consecuencias

- **A favor:** resumen de aprobación exacto, validación fuerte para LLMs, diffs legibles y un destino claro para futuros importadores. Los hallazgos hablan en términos del usuario (`create_order`), no de URLs.
- **En contra:** no hay lógica arbitraria, así que algunos flujos complejos van a requerir varios escenarios o esperar el mini-lenguaje de expresiones. El modo estricto obliga a mantener el JSON Schema siempre al día (se genera desde el código, así que el riesgo es bajo).

## Puntos abiertos

- **Setup por VU** (cada usuario virtual con su propio login): se deja para más adelante, salvo que un caso real lo necesite antes.
- **Datasets** (CSV/JSON): el formato reserva el campo `data` en la raíz.
- **Think time** entre pasos: queda en el backlog; reservar `think` en el paso.
