# ADR-009 — Reporte HTML

Estado: **aceptado** · 17 oct 2026

## Contexto

Los resultados se leen en la terminal (`lt run`, `lt findings`, `lt compare`) o en `results.json`. Para mirar la forma de una corrida (cuándo se rompe, qué tan rápido sube la latencia, si mejoró contra la anterior) hace falta un gráfico. También hace falta algo que se pueda pasar a otra persona sin que tenga `lt` instalado. Además, es el primer paso visual antes de la GUI (principio 4): lo que se resuelva acá se reusa allá.

## Decisión

### Un archivo por corrida, autocontenido

- **Cuándo se genera:** `lt run` escribe `.loadtest/runs/<id>/report.html` al lado de `results.json`. `lt report [corrida] [--open]` lo regenera para cualquier corrida, por ejemplo una vieja, o después de actualizar `lt`.
- **Autocontenido:** es un solo archivo, sin CDN ni recursos externos. Se abre sin conexión y se puede adjuntar o mandar tal cual. Los datos van dentro, en un `<script type="application/json">`, y se escapa `<` para que ningún valor (un body, un mensaje de error del servidor) pueda cerrar el tag.
- **Seguridad:** todo el texto que viene de la corrida entra al DOM con `textContent`, nunca como HTML.
- **Comparación:** si hay una corrida anterior del mismo escenario y entorno, el reporte incluye la comparación (ADR-006). Con la misma carga, también un gráfico del p95 de las dos corridas, segundo a segundo.

### Qué muestra

1. **Encabezado:** escenario, entorno, fecha, veredicto de los thresholds, carga, pausas, ajustes por flag y aprobación.
2. **Indicadores:** requests, iteraciones, errores, descartadas y checks.
3. **Avisos y hallazgos:** cada hallazgo con su severidad (ícono y texto, nunca solo color), la evidencia con nombres en español, el código responsable y las causas posibles, marcadas como hipótesis.
4. **Comparación** con la corrida anterior: veredicto, regresiones, mejoras y p95 por paso.
5. **Por segundo:** iteraciones (iniciadas y descartadas), latencia (p50, p95 y p99), errores si los hubo, y un gráfico chico del p95 de cada paso. La ventana de un hallazgo, como el quiebre, se sombrea.
6. **Tablas:** pasos con percentiles, checks (con motivos si fallaron), thresholds y los datos por segundo. Las tablas son la vista sin gráficos y cubren el contraste bajo de algún color.

### Criterios de los gráficos

- **Un solo eje por gráfico.** Medidas distintas van en gráficos distintos.
- **Interacción:** crosshair con tooltip que muestra todas las series del segundo. Se recorre también con el teclado (flechas).
- **Escala logarítmica opcional en la latencia:** después de un quiebre, la cola de segundos aplasta la línea sana de milisegundos. El botón dice qué hace, y `report.html?log` abre directo en esa escala.
- **Paleta:** las tres primeras posiciones de la paleta categórica de referencia, validadas en claro y oscuro. Los colores de estado (bien, alerta, grave) se reservan para severidad y veredictos.
- **Tema:** modo claro y oscuro según el sistema, más un botón para cambiarlo.

### Por qué HTML generado y no un servidor

Un archivo estático no necesita nada corriendo, se versiona o se adjunta, y no abre puertos. La GUI va a tener su propio proceso. El reporte resuelve "mirar y compartir una corrida" sin depender de ella.

## Consecuencias

- **A favor:** cada corrida tiene su vista gráfica sin pasos extra. Se comparte un archivo. La plantilla (`crates/lt-cli/src/report.html`) es HTML y JavaScript sin dependencias, reutilizable en la GUI.
- **En contra:** el archivo crece con la duración (~1 KB por segundo de corrida). La plantilla está en JavaScript sin tipos: los tests de Rust verifican que los datos entren bien, y la interacción se verificó a mano con Edge headless (captura y mouse simulado por el protocolo de DevTools).
