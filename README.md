# lt — load tester (nombre provisorio)

Herramienta de pruebas de carga en Rust, pensada para usarse junto a Claude Code:
Claude diseña el escenario y analiza los resultados; una persona aprueba y ejecuta la carga.

Estado: **rebanada 2** (Claude diseña). Ver el [roadmap](docs/roadmap.md).

## Uso

```bash
cargo install --path crates/lt-cli    # instala `lt` en ~/.cargo/bin
```

En el repo a probar, creá `.loadtest/config.yaml`:

```yaml
version: 1
default_environment: local
environments:
  local:
    base_url: http://localhost:3000
    allowed_hosts: [localhost]
    limits: { max_rps: 200 }      # opcional: requests/s, max_vus, max_duration
```

y un escenario en `.loadtest/scenarios/smoke.yaml`:

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
lt validate smoke              # --json para agentes
lt run smoke --env local       # resultados en .loadtest/runs/<id>/results.json
```

Si el destino no es `localhost` o una IP de loopback, `lt run` pide tipear el host en una terminal interactiva.

## Con Claude Code

```bash
lt claude install              # en el repo a probar: skill, hook y permisos en .claude/
```

Después, en una sesión de Claude Code: *"armame una prueba de carga del flujo de compra"*. Claude lee el código, escribe el escenario, lo verifica con `lt validate` y `lt check` (solo contra loopback) y te pasa el comando `lt run` para que lo ejecutes vos. No puede ejecutar `lt run` por su cuenta (ADR-001, ADR-004).

Otros comandos: `lt check <escenario>` (smoke check: cada flow una vez, solo loopback) y `lt schema` (JSON Schema del escenario).

Códigos de salida: `0` ok · `1` `check` con pasos fallidos · `2` escenario o configuración inválidos · `3` ejecución rechazada · `4` no se pudieron guardar los resultados.

Formato completo del escenario: [ADR-002](docs/adr/ADR-002-formato-escenario.md).

## Desarrollo

```bash
cargo test --workspace
cargo run --release -p lab-server -- --port 3999   # servidor de laboratorio
```

Para probar contra una API más realista: [shop-api](../shop-api) (repo aparte), con escenarios en su `.loadtest/`.

## Documentación

- [Roadmap](docs/roadmap.md)
- [ADR-001 — Claude-first](docs/adr/ADR-001-claude-first.md)
- [ADR-002 — Formato del escenario](docs/adr/ADR-002-formato-escenario.md)
- [ADR-003 — hyper y scheduler](docs/adr/ADR-003-hyper-y-scheduler.md)
- [ADR-004 — Aprobación y modelo de amenaza](docs/adr/ADR-004-aprobacion.md)
