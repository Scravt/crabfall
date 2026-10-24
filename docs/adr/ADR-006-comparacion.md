# ADR-006 — Comparar corridas

Estado: **aceptado** · 15 oct 2026

## Contexto

El ciclo de uso termina en "arreglé `scryptSync`, ¿mejoró?". Hoy eso se responde mirando dos tablas a ojo. Hace falta que `crabfall` diga qué mejoró, qué empeoró y si el cambio es real o ruido, con los mismos criterios siempre, para que Claude no tenga que adivinar.

## Decisión

`crabfall compare [base] [nueva] [--json]`. Sin argumentos compara la corrida más reciente con la anterior del mismo escenario y entorno. `nueva` es por defecto `latest`. La lógica está en `crabfall-core` (`compare.rs`) y lee `results.json`; no hace falta volver a correr nada.

### ¿Son comparables?

Se comparan escenario, entorno, `load` y el hash de la aprobación (que cubre el plan completo). Cualquier diferencia es un aviso, no un error: a veces se cambia el escenario a propósito. Si `load` es igual, cada segundo de una corrida tiene la misma tasa ofrecida que el mismo segundo de la otra, y se pueden **emparejar segundo a segundo**. Si no, solo se comparan los agregados y se avisa que la diferencia puede ser de la carga. Si alguna corrida tiene `invalid_result`, se avisa primero.

### ¿Real o ruido? (latencia por paso)

El veredicto de cada paso sale del p95:

1. **Magnitud:** cambia ≥ 10% y ≥ 2 ms. Si no, `same`.
2. **Consistencia:** test de signos sobre los segundos emparejados (los que tienen requests del paso en las dos corridas): ¿en cuántos segundos el p95 se movió en la dirección del cambio? Con p < 0,05 (binomial, dos colas) es `better` o `worse`. Si no alcanza, o hay menos de 6 segundos, `uncertain`: "cambió, pero puede ser ruido".
3. Sin emparejamiento (cargas distintas), alcanza con la magnitud y se avisa.

Se eligió el test de signos porque no supone nada sobre la forma de la distribución, se explica en una línea ("empeoró en 52 de 60 segundos") y funciona con lo que ya se guarda (la serie por segundo), sin guardar histogramas.

### Qué cuenta como regresión

- Un paso `worse`.
- Errores: +1 punto porcentual o más (global o por paso).
- Iteraciones descartadas: +1 punto porcentual o más.
- Un hallazgo nuevo de severidad alta o media.
- Un `breaking_point` que persiste pero baja ≥ 10% (el sistema se rompe antes).
- Un threshold que pasaba y ahora falla.

Lo simétrico cuenta como mejora. El veredicto general es `better`, `worse`, `same` o `mixed`.

### Hallazgos

Se emparejan por `id` y paso: `new`, `resolved` o `persists`. Si persiste un `breaking_point`, se muestra la tasa antes y después.

### Salida

- Texto: veredicto, avisos, regresiones y mejoras en una línea cada una, tabla por paso y hallazgos.
- `--json`: lo mismo, estructurado, con la evidencia (segundos a favor y en contra, p-valor).
- Código de salida `1` si hay regresiones (sirve para CI) y `2` si una corrida no existe o se abortó.

## Consecuencias

- Sin histogramas guardados, el test trabaja sobre el p95 por segundo. Con muy pocas requests por segundo, el p95 de cada ventana es ruidoso y el test tiende a `uncertain`: es el error del lado seguro.
- Los umbrales (10%, 2 ms, 1 pp, p < 0,05) son constantes al principio del módulo, como en el diagnóstico.
- Comparar contra una "corrida base" fija (guardada aparte, para CI) queda para después: hoy la base es cualquier corrida en `.crabfall/runs/`.
- Cada paso compara también los bytes por request (ADR-013), enviados y recibidos. Un cambio de al menos ×1,5 (o a la mitad) y 1 KB sale como aviso, no como regresión ni mejora: una respuesta que creció puede explicar un cambio de latencia sin que el servidor haya empeorado. Las corridas sin bytes no se comparan.

## Corrida base fija (20 oct 2026)

- **Fijarla:** `crabfall baseline [corrida]` (por defecto, la más reciente) copia su `results.json` a `.crabfall/baselines/<escenario>.<entorno>.json`. Esa carpeta no está en `.gitignore`: se versiona, así el equipo compara contra la misma referencia. Una corrida abortada no puede ser la base. En la TUI, `b` en el historial hace lo mismo, y el historial marca cuál es.
- **Usarla:** `crabfall compare --baseline [corrida]` compara la corrida (por defecto, la más reciente) contra la base de su escenario y entorno, en lugar de contra la anterior. Sale con 1 si hay regresiones, como siempre. Sin base, sale con 2 y dice cómo fijarla.
- **Por qué no la anterior:** comparar siempre contra la anterior deja pasar regresiones de a poco (cada corrida un poco peor que la otra, nunca lo suficiente). La base fija es la referencia aceptada hasta que alguien decide moverla.
- **Lo que falta para CI:** correr en CI necesita una forma de aprobar sin una persona en una terminal (ADR-004), que no existe. Por ahora la base sirve en la máquina de quien corre.
