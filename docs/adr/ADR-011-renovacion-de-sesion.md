# ADR-011 — Renovación de la sesión de cada VU

Estado: **aceptado** · 18 oct 2026

## Contexto

Con `vu_setup` (ADR-010), cada VU hace login una vez y usa ese token toda la corrida. Pero en las APIs reales el access token vence a los pocos minutos. En `market-api` dura 5 minutos: una corrida más larga empieza a recibir 401 y el reporte muestra una caída que no es del sistema bajo carga, sino del cliente.

## Decisión

### `vu_refresh`

```yaml
vu_setup:
  - name: login
    request: { method: POST, url: /auth/login, json: { ... } }
    extract:
      token: { json: $.access_token }
      refresh_token: { json: $.refresh_token }
vu_refresh:
  every: 4m
  steps:
    - name: refresh
      request:
        method: POST
        url: /auth/refresh
        json: { refresh_token: "{{refresh_token}}" }
      extract:
        token: { json: $.access_token }
        refresh_token: { json: $.refresh_token }
```

- **`every`:** cada cuánto renueva cada VU, contado desde que terminó su `vu_setup` o su última renovación.
- **`steps`:** son opcionales y ven las variables del VU, incluida su fila de cada dataset. Lo que extraen pisa esas variables. Sin `steps`, se repite `vu_setup`, o sea, login de nuevo: sirve para APIs sin refresh token.
- **Restricciones:** requiere `vu_setup`. Sus pasos no llevan `think` y sus nombres son únicos en todo el escenario.
- **Variables nuevas:** lo que extrae la renovación no se suma a lo que ven los flows. Una variable que solo existiera después de renovar faltaría en las primeras iteraciones, así que se rechaza al armar el plan.
- **Por flag:** `--refresh-every 4m` cambia `every`. Si el escenario no tiene `vu_refresh`, crea uno que repite `vu_setup`. Se firma como cualquier ajuste.

### Cuándo corre: en segundo plano

Una tarea aparte renueva cada sesión cuando le toca, esté el VU libre o en una iteración:

- **La iteración en curso no se toca:** tomó sus variables al empezar y sigue con el token anterior, que todavía vale. La siguiente ya toma el nuevo.
- **No le saca capacidad a la carga:** el VU sigue disponible mientras renueva. En el modelo abierto, esto importa: como todos los VUs se preparan juntos, vencen juntos.
- **Concurrencia:** a lo sumo 50 renovaciones a la vez, como en `vu_setup`.
- **Fin de la carga:** después de que termina, no se renueva más.

Se descartaron dos alternativas:

- **Renovar entre iteraciones, sacando al VU del pool:** fue la primera versión. En el modelo abierto, todos los VUs salían a la vez cuando vencían y la carga descartaba iteraciones. Un test lo mostró.
- **Renovar al recibir un 401 y reintentar el paso:** mide un 401 y un reintento que un cliente bien hecho no tendría, y no sirve para APIs que no responden 401 al vencer.

La regla para elegir `every`: menos que la vida del token, menos lo que dura una iteración, porque la iteración usa el token que tenía al empezar.

### Fallas

- **Si una renovación falla** (sin respuesta, status ≥ 400, `extract` o check fallido), ese VU deja de iterar. En el modelo abierto sale del pool. Seguir con un token vencido solo agregaría 401 que no son del sistema.
- **Aviso:** al final, un aviso dice cuántos VUs se perdieron.
- **`crabfall check`:** corre la renovación una vez, después de los flows.

### Firma y resultados

- **Firma:** el plan canónico incluye `vu_refresh` (`every` y pasos) solo si existe, así que las aprobaciones de antes siguen valiendo.
- **Resultados:** `results.json` tiene `vu_refresh` con `every_s`, las renovaciones, los VUs perdidos y, por paso, requests, fallas, percentiles y motivos. No entra en las métricas de los pasos ni en el diagnóstico.
- **Dónde se ve:**
  - `approve`: la cota de requests, VUs × renovaciones posibles × pasos;
  - el encabezado y el resumen de `run`;
  - el reporte HTML, que lo muestra aparte.

## Consecuencias

- **A favor:** corridas largas contra APIs con tokens cortos, sin 401 del cliente y con las métricas limpias. Con refresh token o con login de nuevo.
- **En contra:** la vida del token no se lee de la respuesta (`expires_in`), hay que ponerla a mano en `every`. Si el servidor acorta el TTL, aparecen 401 de nuevo, aunque es justo lo que el reporte tiene que mostrar.

## Diagnóstico de sesiones vencidas

La primera corrida en `market-api` sin `vu_refresh`, con tokens de 60 s, mostró que el diagnóstico de los 401 estaba mal. Decía "falla desde el inicio (200%)" de un paso que había respondido bien durante un minuto. Había dos errores en `error_threshold`:

- **Errores contados dos veces:** un 401 sin el campo que buscaba el `extract` se contaba dos veces en la serie por segundo. Ahora es un error.
- **"Desde el inicio" por error:** con pausas, el paso tenía 2 o 3 requests por segundo y la regla esperaba al primer segundo con 5 o más. Los 401 de los segundos anteriores quedaban como "errores de antes" y parecía que fallaba desde el inicio. Ahora la ventana arranca en el primer segundo que ya fallaba, y la tasa es la de toda la ventana.

Además, si dominan los 401 y empiezan a mitad de corrida, el hallazgo dice "venció la sesión". La primera causa depende de cómo se obtiene el token: con `setup`, pasar el login a `vu_setup`; con `vu_setup`, agregar `vu_refresh`; con `vu_refresh`, bajar `every`.
