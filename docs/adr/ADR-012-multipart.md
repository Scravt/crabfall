# ADR-012 — Bodies multipart (subir archivos)

Estado: **aceptado** · 18 oct 2026

## Contexto

Hasta ahora, un paso podía mandar JSON (`json`) o texto (`body`). Subir archivos, que es común en APIs reales (avatares, adjuntos, importaciones), necesita `multipart/form-data`. `market-api` tiene `POST /files` y no había forma de cargarlo.

## Decisión

### `multipart`

```yaml
request:
  method: POST
  url: /files
  multipart:
    file: { file: files/adjunto.bin, filename: "adjunto-{{$uuid}}.bin", content_type: application/octet-stream }
    description: "subido por {{users.email}}"
```

- **Campos:** cada clave es el nombre de un campo. Un string es un campo de texto y admite `{{variables}}`.
- **Archivos:** un objeto con `file`, que se toma relativo a `.crabfall/` como los datasets, sin rutas absolutas ni `..`.
  - `filename` es opcional, por defecto el nombre del archivo, y admite variables.
  - `content_type` es opcional y por defecto sale de la extensión: los tipos comunes, y si no, `application/octet-stream`.
- **Exclusiones:** excluye a `json` y a `body`. El `content-type` del request lo pone Crabfall, porque lleva el boundary, así que definirlo a mano es un error de validación.
- **Formato:** el body sigue la RFC 7578. En los nombres, las comillas y los saltos de línea se codifican como en los navegadores (`%22`, `%0D`, `%0A`).

### Los archivos

- **Lectura:** se leen una vez, al armar el plan, y se mandan enteros en cada request. No se leen del disco en cada iteración, para que el disco del generador no entre en la medición.
- **Tamaño:** hasta 100 MB por archivo, porque viven en memoria y se copian al armar cada body.
- **Boundary:** es aleatorio y fijo por paso, y se verifica que no aparezca dentro de ningún archivo.
- **Lo que se descartó:** el streaming desde disco, porque complica el cliente y no hace falta con archivos de este tamaño. También un `file` con variables (`files/{{x}}.jpg`), porque obligaría a cargar y firmar un conjunto abierto de archivos. Si hace falta variar el archivo, se usan varios pasos o varios escenarios.

### Firma y resúmenes

- **Firma:** el plan canónico incluye, por campo, el texto (su plantilla) o el archivo, con su ruta, su SHA-256, el `filename` y el `content_type`. Cambiar el contenido del archivo invalida la aprobación, igual que con los datasets (ADR-007). El boundary no se firma porque no está en el escenario. Los escenarios sin `multipart` firman igual que antes.
- **Resúmenes:** `approve` muestra cada archivo con su tamaño y el paso que lo sube (`Archivos  files/adjunto.bin (1.0 MB) en upload_file`), y el encabezado de `run` también. Quien aprueba ve cuántos bytes va a mandar cada request.

## Consecuencias

- **A favor:** se pueden cargar endpoints de subida con archivos reales, y los checks pueden comparar el `sha256` que devuelve el servidor con el del archivo.
- **En contra:** con archivos grandes, el ancho de banda del generador puede ser el límite. Desde el ADR-013 los resultados muestran los bytes por segundo, pero ninguna regla lo diagnostica todavía.
