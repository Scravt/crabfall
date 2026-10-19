# ADR-014 — Cookies, redirects, compresión y formularios

Estado: **aceptado** · 19 oct 2026

## Contexto

El cliente de Crabfall manda exactamente lo que dice el escenario y devuelve la primera respuesta, tal como llega. Con APIs reales eso choca con cuatro cosas que cualquier navegador o cliente HTTP resuelve solo:

- **Cookies:** las APIs con sesión por cookie (Rails, Django, Laravel, muchos BFF) necesitaban un `extract` del `set-cookie` y un header `cookie` a mano en cada paso.
- **Redirects:** no se seguían; un 302 después de un login quedaba como una respuesta no-2xx.
- **Compresión:** el servidor no comprimía porque nadie se lo pedía, así que se medía un tráfico que los clientes reales no generan. Y si el escenario lo pedía, los checks fallaban sobre un body comprimido.
- **Formularios:** un body `application/x-www-form-urlencoded` había que armarlo a mano con `body` y el header.

## Decisión

Por defecto, Crabfall se comporta como un cliente real. Cada comportamiento se puede apagar.

### Cookies

- **Un jar por VU:** cada VU guarda las cookies que le manda el servidor y las reenvía, con las reglas de un navegador (dominio, path, vencimiento, `Secure` solo por HTTPS). Lo implementa `cookie_store` (RFC 6265).
- **Duración:** el jar vive desde el `vu_setup` del VU hasta su última iteración, y la renovación (`vu_refresh`) usa el mismo jar. Las cookies que se setean en una iteración siguen en la próxima del mismo VU, como un usuario que vuelve. `setup` usa un jar propio, que no es de ningún VU.
- **Header propio:** un header `cookie` del escenario se manda junto con las del jar, no en su lugar.
- **Configuración:** `cookies: false` en la raíz del escenario lo apaga. Se firma solo si está en `false`.
- **Corrección de paso:** sin `vu_setup`, todos los VUs compartían una misma sesión vacía. Ahora cada VU tiene la suya, para que las cookies no se mezclen.

### Redirects

- **Cuántos:** cada paso sigue hasta 10 redirects (301, 302, 303, 307 y 308). `redirects: N` en el request o en `defaults` cambia el número; `0` no sigue ninguno. El tope de validación es 30.
- **A dónde:** solo hacia hosts de `allowed_hosts`. Un redirect hacia otro host es una falla de tipo `redirect` y no se manda.
- **Método y body:** 303, y 301 o 302 después de un POST, pasan a GET sin body, como los navegadores. 307 y 308 repiten el método y el body.
- **Credenciales:** el `authorization` y la cookie propia del escenario no viajan a otro host. Las del jar siguen sus propias reglas de dominio.
- **Métricas:** la latencia del paso incluye todos los saltos, el timeout cubre la cadena entera y el status es el de la última respuesta. Los bytes suman todos los saltos. `crabfall check` muestra cada salto.
- **Firma:** `redirects` se firma solo si no es 10.

### Compresión

- **Qué se pide:** cada request manda `accept-encoding: gzip, br`, como los clientes reales. Un header `accept-encoding` del escenario lo pisa; `identity` pide sin comprimir.
- **Qué se descomprime:** el body se descomprime según `content-encoding` (gzip, deflate o br) para que checks y `extract` vean el contenido, hasta 64 MB descomprimidos. Más que eso es una falla de tipo `protocol`.
- **Bytes:** los bytes medidos (ADR-013) siguen siendo los del cable, es decir, los comprimidos.
- **Firma:** el header va en la lista de headers del paso, así que se firma como el `user-agent`. Las aprobaciones anteriores a este cambio dejan de valer.

### Formularios

`form: { user: "{{users.email}}", remember: true }` arma un body `application/x-www-form-urlencoded` con los campos en el orden del escenario. Los valores son texto (admite variables), números o booleanos. Excluye a `json`, `body` y `multipart`.

### `crabfall check` no sale de loopback

Un redirect podía llevar el smoke check a otro host de `allowed_hosts`. Ahora el cliente de `check` se niega a conectarse a cualquier dirección fuera de loopback, venga de donde venga el request (ADR-004 §4).

### Lo que ve quien aprueba

`approve` y el encabezado de `run` muestran una línea `HTTP`, por ejemplo `cookies por VU · sigue hasta 10 redirects (solo a allowed_hosts) · acepta gzip, br`.

## Consecuencias

- **A favor:** las APIs con sesión por cookie, los logins que redirigen y los servidores que comprimen funcionan sin configuración extra. Los bytes por segundo reflejan lo que mandaría un cliente real.
- **En contra:** cambian el tráfico de los escenarios existentes (más headers, y redirects que antes no se seguían), así que hay que volver a aprobarlos. No hay un jar "limpio" por iteración: un escenario que necesite empezar cada iteración sin cookies tiene que usar `cookies: false` y manejarlas a mano.
