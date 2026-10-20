# ADR-015 — TUI en la terminal

Estado: **aceptado** · 19 oct 2026

## Contexto

Hoy Crabfall se usa por comandos (`approve`, `run`, `findings`, `compare`), con Claude diseñando y analizando, y con un reporte HTML por corrida. El plan original era una GUI con Tauri; se cambió por una TUI en la terminal, al menos por ahora (roadmap, 19 oct 2026). Falta un lugar para operar sin recordar flags y, sobre todo, para ver una corrida mientras pasa: hoy solo hay una línea de progreso.

## Decisión

### Quién hace qué

| | Para qué | Cuándo |
| --- | --- | --- |
| TUI | Operar y mirar en vivo: elegir, ajustar, probar, aprobar, correr, frenar | Antes y durante |
| Reporte HTML (ADR-009) | Analizar en detalle y compartir | Después |
| Claude (ADR-001) | Diseñar escenarios y explicar hallazgos desde el código | Antes y después |

La TUI no copia los gráficos del HTML: lo abre con una tecla. Lo que aporta es lo que el HTML no puede, la corrida en vivo.

### Cómo se abre

`crabfall` sin argumentos, con una terminal interactiva, abre la TUI. Sin terminal (Claude, CI, un pipe) muestra la ayuda como hasta ahora. Los subcomandos no cambian. El hook de Claude Code bloquea también `crabfall` a secas.

### Funcionalidad (primera versión)

- **Elegir:** escenarios de `.crabfall/` con su descripción, el estado de la aprobación por entorno (aprobado, cambió desde la aprobación, sin aprobar) y la última corrida. El entorno se elige arriba.
- **Ajustar:** los mismos ajustes que los flags (ADR-008), con el mismo tipo `Overrides` de `crabfall-core`. Se validan mientras se escribe, con el error en el campo, y se muestra el comando equivalente para copiarlo o pasárselo a Claude. No edita el YAML.
- **Probar:** el smoke check, solo contra loopback como siempre (ADR-004 §4).
- **Aprobar:** el mismo resumen de riesgo que `crabfall approve` (con el diff si cambió) y se tipea el host. La TUI es una terminal con una persona, así que cumple el ADR-004; Claude no puede usarla.
- **Correr y frenar:** la vista en vivo de abajo, con `q` para frenar con confirmación.
- **Resultados:** el resumen al terminar, con `o` para abrir el `report.html`.
- **Después:** el historial de corridas y la comparación entre dos, en una segunda rebanada. Mientras tanto están `crabfall compare` y el HTML.

### Durante la corrida

- **Encabezado:** escenario y entorno, avance, fase (`vu_setup`, carga o graceful stop), la aprobación y la línea `HTTP`.
- **Indicadores del último segundo, frente a lo pedido:** iteraciones/s y descartadas, VUs ocupados, requests/s, errores, checks y bytes por segundo.
- **Sparklines:** p95 y errores por segundo, para ver la forma (si sube, si se rompe).
- **Pasos:** requests, error %, p50 y el p95 de los últimos 5 s (no el acumulado, para que se vea el cambio), y los códigos.
- **Avisos en vivo:** scheduler tarde, iteraciones descartadas, VUs fuera de `vu_setup`, fallas de renovación. Los hallazgos salen al final: el diagnóstico necesita la corrida entera.

### Al terminar

- **Pestañas:** resumen (veredicto, thresholds, totales, tráfico y la comparación rápida con la anterior), hallazgos (evidencia, causas, archivo de código), pasos (percentiles, códigos, límite declarado, bytes por request) y checks fallidos.
- **Teclas:** `o` abre el reporte, `r` vuelve a correr con los mismos ajustes, `c` compara (cuando exista el historial).

### Lo que necesita el motor

- **Frenar en orden:** dejar de iniciar iteraciones, esperar las que están en curso (graceful stop) y guardar `results.json` y el reporte, marcados como interrumpidos por la persona. El diagnóstico corre sobre lo que hubo. `crabfall run` también lo usa con Ctrl+C.
- **La serie en vivo:** el colector publica cada segundo el punto que cerró (global y por paso), no solo al final.

### Biblioteca

`ratatui` con `crossterm`: es la opción estándar en Rust y funciona en Windows, Linux y macOS.

## Consecuencias

- **A favor:** un flujo completo sin recordar flags, y la corrida en vivo, que hoy no existe. Frenar con resultados también mejora `crabfall run`.
- **En contra:** una segunda interfaz para mantener al día con cada ajuste nuevo; por eso usa los mismos `Overrides` y el mismo resumen de aprobación que la CLI, no copias.
