# Crabfall

Herramienta de pruebas de carga en Rust, pensada para usarse junto a Claude Code:
Claude diseña el escenario y analiza los resultados; una persona aprueba y ejecuta la carga.

Estado: **rebanada 15 cerrada** (cliente HTTP real y lo que dice el servidor), después de la v0.1.0. Ya funcionan: diseño con Claude, aprobación firmada, escenarios con `setup`, `extract`, checks y thresholds, tres executors, diagnóstico, comparación de corridas, datasets, pausas entre pasos, ajustes de la corrida por flag, un reporte HTML por corrida, login por usuario virtual (`vu_setup`), renovación de tokens durante la carga (`vu_refresh`), subida de archivos (`multipart`), bytes enviados y recibidos por segundo, y un cliente HTTP con cookies, redirects, compresión y formularios. Ver el [roadmap](docs/roadmap.md).

## Uso

```bash
cargo install --path crates/crabfall-cli    # instala `crabfall` en ~/.cargo/bin
```

En el repo a probar, creá `.crabfall/config.yaml`:

```yaml
version: 1
default_environment: local
environments:
  local:
    base_url: http://localhost:3000
    allowed_hosts: [localhost]
    limits: { max_rps: 200 }      # opcional: requests/s, max_vus, max_duration
```

y un escenario en `.crabfall/scenarios/smoke.yaml`:

```yaml
version: 1
name: smoke
flows:
  main:
    steps:
      - name: list_products
        request: { method: GET, url: /products }
load:
  executor: constant_rate
  rate: 50/s
  duration: 30s
  max_vus: 50
```

```bash
crabfall validate smoke              # --json para agentes
crabfall check smoke                 # cada flow una vez, solo contra loopback
crabfall approve smoke --env local   # resumen de riesgo; pide tipear el host
crabfall run smoke --env local       # resultados en .crabfall/runs/<id>/results.json
```

`crabfall run` solo ejecuta escenarios aprobados. La aprobación firma el escenario junto con el entorno resuelto: cualquier cambio en el tráfico la invalida y `crabfall run` muestra qué cambió. Se aprueba en una terminal interactiva; la clave de firma vive en el keyring del sistema ([ADR-004](docs/adr/ADR-004-aprobacion.md)).

Para que cada iteración use datos distintos (usuarios, IDs), declarás archivos CSV o JSON en `data:` y los usás como `{{dataset.columna}}`:

```yaml
data:
  users: { file: data/users.csv }    # .crabfall/data/users.csv, con encabezado user,password
flows:
  main:
    steps:
      - name: login
        request:
          method: POST
          url: /auth/login
          json: { user: "{{users.user}}", password: "{{users.password}}" }
        think: 1s-3s                  # pausa después del paso, como una persona
```

Cada iteración toma una fila, en orden o al azar (`pick: random`). Editar el archivo invalida la aprobación ([ADR-007](docs/adr/ADR-007-datasets.md)). `think` también va en `defaults` para todos los pasos; no cuenta en la latencia y `crabfall check` lo ignora.

Para APIs con sesión, `vu_setup` corre una vez por usuario virtual, antes de la carga: cada VU hace su login con su fila del dataset y usa su token en todas sus iteraciones, sin medir el login en cada una:

```yaml
vu_setup:
  - name: login
    request: { method: POST, url: /auth/login, json: { user: "{{users.user}}", password: "{{users.password}}" } }
    extract: { token: { json: $.access_token } }
defaults:
  headers: { Authorization: "Bearer {{token}}" }
```

Se prepara un VU por cada `vus` (o `max_vus`); el resumen de `crabfall approve` muestra cuántos logins son ([ADR-010](docs/adr/ADR-010-setup-por-vu.md)).

Si el token vence durante la carga, `vu_refresh` lo renueva en segundo plano, sin cortar iteraciones ni sumarse a sus métricas. Con un refresh token:

```yaml
vu_refresh:
  every: 4m             # menos que la vida del token
  steps:
    - name: refresh
      request: { method: POST, url: /auth/refresh, json: { refresh_token: "{{refresh_token}}" } }
      extract: { token: { json: $.access_token }, refresh_token: { json: $.refresh_token } }
```

Sin `steps`, repite `vu_setup` (login de nuevo). `--refresh-every 4m` lo ajusta por flag. Un VU que no puede renovar deja de iterar ([ADR-011](docs/adr/ADR-011-renovacion-de-sesion.md)).

Para subir archivos, `multipart` arma un body `multipart/form-data` con archivos de `.crabfall/` y campos de texto:

```yaml
request:
  method: POST
  url: /files
  multipart:
    file: { file: files/adjunto.bin, filename: "adjunto-{{$uuid}}.bin" }
    description: "subido por {{users.email}}"
```

El archivo se lee una vez y va entero en cada request (hasta 100 MB). El tipo sale de la extensión, o de `content_type`. La aprobación firma su contenido ([ADR-012](docs/adr/ADR-012-multipart.md)).

Con archivos grandes conviene mirar el tráfico: `crabfall run` muestra los bytes enviados y recibidos por el socket (headers, body y TLS), con el promedio y el pico por segundo, y el reporte los grafica ([ADR-013](docs/adr/ADR-013-trafico.md)):

```
tráfico      enviados 137.4 MB (2.1 MB/s, pico 4.3 MB/s = 36 Mbit/s)  recibidos 137.6 MB (2.1 MB/s, pico 4.1 MB/s = 34 Mbit/s)
```

### Cookies, redirects y compresión

Crabfall se comporta como un cliente real ([ADR-014](docs/adr/ADR-014-cliente-http-real.md)):

- **Cookies:** cada VU guarda las cookies que le manda el servidor y las reenvía, desde su `vu_setup` hasta su última iteración. Una API con sesión por cookie funciona sin `extract`. `cookies: false` en la raíz las apaga.
- **Redirects:** cada paso sigue hasta 10, solo hacia hosts de `allowed_hosts`; la latencia incluye todos los saltos. `redirects: 0` en el request (o en `defaults`) no sigue ninguno.
- **Compresión:** cada request pide `gzip, br`; checks y `extract` ven el body descomprimido y los bytes cuentan lo comprimido. `accept-encoding: identity` en los headers pide sin comprimir.

Para un formulario clásico, `form` arma el body `application/x-www-form-urlencoded`:

```yaml
request:
  method: POST
  url: /login
  form: { username: "{{users.email}}", password: "{{users.password}}", remember: true }
```

`crabfall approve` muestra cómo se va a comportar el cliente en una línea `HTTP`.

### Ajustes sin editar el YAML

`validate`, `check`, `approve` y `run` aceptan flags que pisan al escenario para esa corrida. Son lo que tendría un panel de personalización:

```bash
crabfall run checkout --env local --rate 100/s --duration 2m --think 1s-3s
crabfall run checkout --executor constant_vus --vus 20 --duration 5m
crabfall run checkout --executor ramping_rate --stages 50/s:1m,200/s:3m,0/s:30s
crabfall run login-users --data users=data/otros.csv --pick users=random
crabfall run shop-per-user --duration 10m --refresh-every 4m
crabfall run checkout --threshold "http.latency.p95<300ms" --threshold "http.error_rate<1%"
```

Lista completa en `crabfall run --help`. Los que cambian el tráfico los firma la aprobación: pasá los mismos a `crabfall approve` y a `crabfall run`. `--threshold` reemplaza a los del YAML y no hace falta volver a aprobar ([ADR-008](docs/adr/ADR-008-think-time-y-ajustes.md)).

## Con Claude Code

```bash
crabfall claude install              # en el repo a probar: skill, hook y permisos en .claude/
```

Después, en una sesión de Claude Code: *"armame una prueba de carga del flujo de compra"*. Claude lee el código, escribe el escenario, lo verifica con `crabfall validate` y `crabfall check` (solo contra loopback) y te pasa los comandos `crabfall approve` y `crabfall run` para que los ejecutes vos. No puede aprobar ni ejecutar por su cuenta (ADR-001, ADR-004).

`crabfall findings [--json]` muestra los hallazgos de la última corrida (o de una por id). `crabfall compare [base] [nueva] [--json]` compara dos corridas (por defecto, la última con la anterior del mismo escenario): qué mejoró, qué empeoró y si el cambio es consistente o puede ser ruido ([ADR-006](docs/adr/ADR-006-comparacion.md)). `crabfall schema` imprime el JSON Schema del escenario.

Cada `crabfall run` deja además `report.html` al lado de `results.json`: un archivo que se abre sin conexión, con gráficos por segundo (iteraciones, latencia, p95 por paso), los hallazgos con la ventana sombreada, la comparación con la corrida anterior y las tablas. `crabfall report [corrida] --open` lo regenera y lo abre en el navegador ([ADR-009](docs/adr/ADR-009-reporte-html.md)).

Códigos de salida: `0` ok · `1` `check` con pasos fallidos, thresholds no cumplidos, `setup` fallido o `compare` con regresiones · `2` escenario o configuración inválidos · `3` rechazado (sin aprobación válida, host no permitido, límites, sin terminal) · `4` error al guardar o al acceder al keyring.

Formato completo del escenario: [ADR-002](docs/adr/ADR-002-formato-escenario.md).

## Desarrollo

```bash
cargo test --workspace
cargo run --release -p lab-server -- --port 3999   # servidor de laboratorio
```

El CI (`.github/workflows/ci.yml`) corre `cargo fmt --check` y `clippy` en Linux, y los tests en Linux y Windows, en cada push a `main` o `dev` y en cada pull request. Los warnings cuentan como errores. En Windows se saltean las patologías del diagnóstico (`CRABFALL_SKIP_TIMING_TESTS=1`): miden tiempos reales y ese runner, de 2 núcleos, no sostiene el calendario de la carga.

Para probar contra APIs más realistas: [shop-api](../shop-api) (local, con problemas de performance plantados) y [market-api](../market-api) (Postgres, JWT, rate limit y uploads), repos aparte con escenarios en su `.crabfall/`.

## Documentación

- [Roadmap](docs/roadmap.md)
- [ADR-001 — Claude-first](docs/adr/ADR-001-claude-first.md)
- [ADR-002 — Formato del escenario](docs/adr/ADR-002-formato-escenario.md)
- [ADR-003 — hyper y scheduler](docs/adr/ADR-003-hyper-y-scheduler.md)
- [ADR-004 — Aprobación y modelo de amenaza](docs/adr/ADR-004-aprobacion.md)
- [ADR-005 — Diagnóstico](docs/adr/ADR-005-diagnostico.md)
- [ADR-006 — Comparar corridas](docs/adr/ADR-006-comparacion.md)
- [ADR-007 — Datasets](docs/adr/ADR-007-datasets.md)
- [ADR-008 — Think time y ajustes por flag](docs/adr/ADR-008-think-time-y-ajustes.md)
- [ADR-009 — Reporte HTML](docs/adr/ADR-009-reporte-html.md)
- [ADR-010 — Setup por VU](docs/adr/ADR-010-setup-por-vu.md)
- [ADR-011 — Renovación de sesión](docs/adr/ADR-011-renovacion-de-sesion.md)
- [ADR-012 — Subir archivos (multipart)](docs/adr/ADR-012-multipart.md)
- [ADR-013 — Bytes enviados y recibidos](docs/adr/ADR-013-trafico.md)
- [ADR-014 — Cookies, redirects, compresión y formularios](docs/adr/ADR-014-cliente-http-real.md)
