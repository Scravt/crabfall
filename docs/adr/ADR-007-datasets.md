# ADR-007 — Datasets

Estado: **aceptado** · 16 oct 2026

## Contexto

Hoy todas las iteraciones mandan lo mismo: un usuario de `secrets`, IDs de `vars` o de `{{$randomInt}}`. Contra una API real eso no alcanza. Si todos entran con el mismo usuario, se mide la caché de una sola sesión, se chocan los locks de una misma fila y no se parece al tráfico real. ADR-002 reservó el campo `data` para resolverlo.

## Decisión

### Formato

```yaml
data:
  users: { file: data/users.csv }                     # pick: sequential (por defecto)
  products: { file: data/products.json, pick: random }

flows:
  main:
    steps:
      - name: login
        request:
          method: POST
          url: /auth/login
          json: { user: "{{users.user}}", password: "{{users.password}}" }
      - name: get_product
        request: { method: GET, url: "/products/{{products.id}}" }
```

- `file` es relativo a `.crabfall/`. Se aceptan dos formatos:
  - **`.csv` con encabezado.** Todas las celdas son strings: inferir tipos rompe valores como `01234`.
  - **`.json` con un arreglo de objetos.** Conserva los tipos. Las columnas son las claves del primer objeto, y todas las filas tienen que tener exactamente esas claves.
- Los nombres de columna y de dataset usan letras, números y `_`. El dataset no puede llamarse `env`, porque `{{env.X}}` ya son los secretos.
- `pick`:
  - `sequential`: las filas en orden, compartidas entre todos los VUs. Al terminar, vuelve a empezar.
  - `random`: una fila al azar en cada iteración.

### Una fila por iteración

Cada iteración toma **una** fila de cada dataset que usa su flow y la ve en todos sus pasos como `{{dataset.columna}}`. Así el email y la contraseña siempre son de la misma fila. Un flow que no usa un dataset no consume filas, así que la secuencia no se saltea filas por culpa de otros flows. `crabfall check` usa la primera fila con `sequential`.

La sintaxis `{{dataset.columna}}` no choca con nada: los nombres de variable y de `extract` no llevan puntos, y `env.` ya está tomado por los secretos. Con un valor que es exactamente `"{{users.id}}"`, el tipo se conserva como en cualquier variable (ADR-002 §6).

### No se usan en `setup`

`setup` corre una sola vez, así que una fila ahí no tendría sentido. Si `setup` usa una columna, es un error que lo explica. Un login por usuario y por VU es otra cosa ("setup por VU") y queda en el backlog: hoy se hace con el login como primer paso del flow, como en el ejemplo.

### La aprobación firma el contenido

Cambiar el archivo cambia el tráfico: otros usuarios, otros IDs. Por eso el plan canónico (ADR-004) incluye, por cada dataset, `name`, `file`, `pick` y el **SHA-256 del archivo**, pero no su contenido, que puede ser grande. Si se edita el CSV, `crabfall run` rechaza la aprobación igual que si se editara el escenario. La clave `data` solo aparece en el plan canónico cuando hay datasets, para no invalidar las aprobaciones de escenarios que no los usan.

El resumen de `crabfall approve` y el encabezado de `crabfall run` muestran cada dataset con su archivo, cantidad de filas y `pick`.

### Errores

`crabfall validate` (con proyecto), `crabfall check`, `crabfall approve` y `crabfall run` leen los archivos y reportan:

- archivo inexistente, sin filas, con una extensión que no es `.csv` ni `.json`, o que no es UTF-8;
- columnas repetidas o con nombres inválidos, y filas con más o menos celdas (con el número de fila);
- `{{users.pass}}` cuando `users` no tiene esa columna, listando las que tiene;
- `{{x.y}}` cuando no existe el dataset `x`;
- columnas usadas en `setup`.

## Alternativas descartadas

- **Columnas sueltas (`{{email}}`), como Artillery:** dos datasets con la misma columna chocan, y en el YAML no se ve de dónde sale cada valor.
- **Inferir tipos en CSV:** `01234` pasaría a ser `1234` y `true` a ser un booleano sin que nadie lo pida. Para tipos, JSON.
- **`pick: unique`** (cada fila una sola vez y cortar al agotarse): obliga a definir qué pasa con las iteraciones que no tienen fila, y cómo se cuentan. Se agrega cuando un caso real lo pida, por ejemplo registrar usuarios nuevos.
- **Firmar el contenido completo:** el hash alcanza para detectar el cambio. Para ver *qué* cambió en el archivo está git.

## Consecuencias

- **A favor:** escenarios con usuarios e IDs reales sin lógica en el YAML. La firma cubre los datos. Los errores nombran la columna y la fila.
- **En contra:** el archivo entero se carga en memoria (está bien hasta decenas de MB). Los datos de prueba suelen tener credenciales de usuarios de prueba: si no se quieren versionar, hay que agregar el archivo al `.gitignore` del proyecto. `crabfall` no lo decide.

## `pick: unique` (20 oct 2026)

- **Qué hace:** cada fila se usa una sola vez en toda la corrida, por ejemplo para registrar usuarios o consumir cupones. El contador es del plan: lo comparten todos los VUs.
- **Cuando se acaban:** la fila se toma al despachar la iteración, antes de contarla como iniciada. Si no quedan, la carga termina en orden (no se inician más iteraciones y se espera a las que están en curso), con `data_exhausted` (dataset y segundo) en `results.json` y un aviso al principio. No cuenta como falla: es el final natural de "una fila por iteración". Tampoco es un freno de la persona (`stopped_at_s` queda vacío).
- **En `vu_setup`:** cada VU toma una fila distinta, así que el plan exige al menos una por VU; si no, no se arma.
- **Para quien aprueba:** si el dataset tiene menos filas que las iteraciones planificadas, el resumen lo dice ("unique, 50 filas para ≈600 iteraciones: la carga termina cuando se acaben").
