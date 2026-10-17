# ADR-008 — Think time y ajustes por flag

Estado: **aceptado** · 17 oct 2026

## Contexto

Dos cosas faltaban para que una corrida se parezca a una persona usando la API y para que se pueda ajustar sin editar el YAML:

1. **Pausas.** Hoy un VU encadena los pasos sin respiro. Con `constant_vus`, 10 VUs pueden generar cientos de requests por segundo, cuando 10 personas reales generan unas pocas.
2. **Ajustes.** Probar "lo mismo, pero con el doble de tasa" obliga a editar el escenario. Además, la idea es que más adelante haya una interfaz gráfica con un panel de personalización. Ese panel tiene que poder expresar todo lo que ajusta con los mismos parámetros que la CLI, para que la GUI sea una capa delgada (ADR-001).

## Decisión

### Think time

```yaml
defaults:
  think: 1s-3s              # para todos los pasos de los flows
flows:
  main:
    steps:
      - name: list_products
        request: { method: GET, url: /products }
        think: 5s           # este paso pisa a defaults; `0s` lo quita
```

- **Valor:** `2s` es una pausa fija. `1s-3s` es una pausa al azar en ese rango, con distribución uniforme.
- **Cuándo:** la pausa va **después** del paso, incluido el último, porque es lo que hace una persona que lee la página antes de seguir.
- **Ocupa el VU, pero no cuenta en la latencia.** El paso siguiente mide desde que termina la pausa. En el modelo abierto, la pausa cuenta para `max_vus`: el skill lo tiene en cuenta al calcularlo.
- **Dónde no aplica:** en `setup` no se permite, porque corre una sola vez. `lt check` ignora las pausas, para que el smoke check siga siendo rápido.
- **Firma:** la aprobación firma la pausa de cada paso. Solo aparece en el plan canónico si el paso tiene pausa, así que las aprobaciones de antes siguen valiendo.
- **Comparación:** `results.json` guarda las pausas en `load.think`. Así, `lt compare` no empareja segundo a segundo dos corridas con pausas distintas, porque la carga ofrecida no es la misma.

### Ajustes por flag

Criterio: entra por flag todo lo que tendría sentido en un panel de personalización de una corrida. No entra lo que define *qué* se prueba (pasos, requests, checks), porque eso es el escenario.

| Panel | Flags |
| --- | --- |
| Entorno y variables | `--env`, `--var nombre=valor` (ya existían) |
| Perfil de carga | `--executor`, `--rate`, `--duration`, `--vus`, `--max-vus`, `--start-rate`, `--stages 100/s:1m,0/s:30s`, `--graceful-stop` |
| Pausas | `--think 1s-3s` (pisa a todas; `0s` las quita) |
| Datos | `--data users=data/otros.csv`, `--pick users=random` |
| Criterios | `--threshold "http.latency.p95<300ms"`, `--threshold "http.latency.p99{step=login}<1s"` (reemplazan a los del YAML; no se firman) |

- **Comandos:** los cuatro que arman el plan aceptan los mismos flags: `validate`, `check`, `approve` y `run`. Una GUI valida con los mismos parámetros con los que después corre.
- **Cuándo se aplican:** sobre el escenario parseado, **antes** de armar el plan, y después se vuelven a correr las reglas de `validate`. Por eso valen las mismas reglas que en el YAML, y el error dice qué flag lo corrige: `falta stages (usá --stages)`, o `duration no aplica a ramping_rate (sacá --duration)`.
- **`--executor`** descarta los campos de `load` que no le aplican. Por ejemplo, `--executor constant_vus` saca `rate` y `max_vus` y conserva `duration`.
- **Firma:** como cambian el plan, la aprobación los firma igual que a `--var`. Hay que pasar los mismos flags a `approve` y a `run`; si no coinciden, `run` rechaza y muestra el diff del plan. La aprobación guarda además, sin firmar, los flags con los que se aprobó. Si `run` llega con otros, muestra los dos juegos y los comandos para correr lo aprobado o aprobar lo nuevo. Ese campo es solo una ayuda: lo que autoriza sigue siendo el hash.
- **Visibilidad:** el resumen de `approve` los muestra en la línea `Ajustes`, para que quien aprueba vea que la corrida no es exactamente el YAML. `run` los muestra en el encabezado y `results.json` los guarda en `tweaks`.
- **En el código:** `lt-core` tiene el tipo `Overrides` (`overrides.rs`) y la CLI solo parsea. Una GUI, o un servidor MCP, usan el mismo tipo sin pasar por la CLI.

### Fuera por ahora

- **Elegir flows o cambiar sus pesos** (`--flow buy=50`): cambia *qué* se prueba más que *cómo*. Se evalúa con un caso real.

## Consecuencias

- **A favor:** corridas más realistas con `constant_vus`, y una sola definición de qué se puede ajustar, compartida entre la CLI, la GUI y Claude. Los errores nombran el flag.
- **En contra:** un escenario más los ajustes es más difícil de leer que el YAML solo. Por eso los ajustes se muestran en `approve`, en `run` y en `results.json`.
