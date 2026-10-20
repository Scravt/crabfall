# ADR-016 — Idioma de la salida

Estado: **aceptado** · 19 oct 2026

## Contexto

Todo lo que imprimía Crabfall estaba en español: la terminal, los errores de validación, los hallazgos, la ayuda y el reporte HTML. Para publicarlo hace falta inglés, y conviene decidirlo antes de la TUI (ADR-015) para no traducirla dos veces.

## Decisión

### Idiomas y cómo se elige

- **Idiomas:** inglés y español. **Por defecto, inglés**, siempre: no se toma el idioma del sistema operativo.
- **Prioridad:** `--lang en|es` (en cualquier comando, antes o después del subcomando) > la variable `CRABFALL_LANG` > `language:` en `.crabfall/config.yaml` (por proyecto) > inglés.
- **Resolución temprana:** se resuelve al arrancar, antes de que clap arme la ayuda, así que la ayuda también sale en el idioma elegido.

### Qué se traduce

- **La salida de la herramienta:** terminal, errores de validación y de plan, hallazgos y sus causas, avisos, la comparación, la ayuda de los comandos y el reporte HTML (que también formatea números y fechas según el idioma).
- **`results.json`:** los textos (títulos de hallazgos, causas, avisos) quedan en el idioma de la corrida; las claves son siempre las mismas. El reporte se arma en el idioma de quien lo genera.
- **Lo que no cambia:** las claves del YAML, las claves del JSON, los nombres de comandos y flags, las etiquetas técnicas de los checks (`status in`, `body contains`) y los ids de los hallazgos.
- **Documentación:** el README y la skill de Claude están en inglés (los lee alguien de afuera, o Claude, que igual contesta en el idioma de la persona). La documentación del schema (`crabfall schema`) también. Los ADRs y el roadmap siguen en español.

### Cómo está hecho

- **Los dos textos juntos:** cada mensaje tiene sus dos versiones en el mismo lugar: `tr!("english {x}", "español {x}")`, o `pick(en, es)` para textos fijos. Así no se puede agregar un mensaje en un idioma y olvidar el otro. No hay archivos de traducción aparte.
- **Ayuda:** clap la toma de los comentarios de documentación, que quedan en inglés; una tabla en `main.rs` aplica el español cuando corresponde.
- **Reporte:** el idioma viaja en los datos embebidos y el JavaScript elige con `L(en, es)`.
- **Tests:** los tests existentes verifican los textos en español, así que el workspace los corre con `CRABFALL_LANG=es` (`.cargo/config.toml`). El inglés tiene sus propios tests (`tests/english.rs` en los dos crates). Un `crabfall` instalado no lee ese archivo y arranca en inglés.

## Consecuencias

- **A favor:** la herramienta se puede publicar en inglés sin perder el español, y la TUI nace bilingüe.
- **En contra:** cada mensaje nuevo se escribe dos veces. Un idioma más (por ejemplo, portugués) obligaría a pasar de pares a catálogos.
