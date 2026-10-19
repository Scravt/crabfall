# ADR-013 — Bytes enviados y recibidos

Estado: **aceptado** · 19 oct 2026

## Contexto

Con `multipart` (ADR-012) lt manda archivos grandes. Si la latencia sube, puede ser el servidor o puede ser el ancho de banda entre el generador y el servidor, y hasta ahora los resultados no mostraban cuántos bytes se movían.

## Decisión

### Qué se mide

- **Los bytes del socket**, no los del body: un envoltorio sobre el `TcpStream` cuenta lo que se lee y se escribe. Así entran los headers, el envoltorio multipart, el chunked y, con HTTPS, el cifrado y el handshake de TLS. Es lo que ocupa el enlace, que es lo que interesa al hablar de ancho de banda.
- **Por request:** cada request lleva su medidor y lo engancha a la conexión mientras la usa. HTTP/1.1 manda un request a la vez por conexión, así que todo lo que pasa por el socket en ese momento es suyo. Una conexión nueva le suma al primer request su handshake.
- **Aunque no haya respuesta:** el medidor es del request, no de la respuesta. Si un request vence por timeout después de mandar el archivo, esos bytes cuentan. Es justo el caso que importa cuando el enlace está saturado.
- **Solo la carga:** los requests de `setup`, `vu_setup` y `vu_refresh` no entran, igual que en las demás métricas.

### Dónde aparece

- `results.json`: `bytes_sent` y `bytes_received` por paso y por segundo en la serie, y en `totals` junto con `send_rate` y `receive_rate` (bytes por segundo, promedio de la corrida). Las corridas viejas no los tienen y se leen como 0.
- `lt run`: una línea `tráfico` con el total, el promedio y el pico por segundo, también en Mbit/s porque así se miden los enlaces.
- El reporte: un tile, un gráfico por segundo y los bytes por request de cada paso.

### Lo que se descartó

- **Contar el body:** es exacto y fácil de testear, pero deja afuera headers y TLS y no dice nada de un timeout a mitad de envío.
- **Un contador global muestreado:** no separa por paso y mezcla el tráfico de `vu_setup` y `vu_refresh` con el de la carga.

## Consecuencias

- **A favor:** se ve si el tráfico llega a un techo mientras la latencia sube, y cuántos bytes mueve cada paso.
- **En contra:** lt no conoce la capacidad del enlace, así que todavía no hay una regla de diagnóstico que diga "ancho de banda". `lt compare` tampoco compara bytes por request: una respuesta que creció mucho entre dos corridas no se marca. Los dos quedan en el backlog.
