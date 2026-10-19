# ADR-010 — Setup por VU

Estado: **aceptado** · 17 oct 2026

## Contexto

Con lo que había, una API con sesión se probaba de dos formas, y las dos estaban mal:

- **Login en `setup`:** una sola vez, así que todos los VUs comparten el mismo token. Se mide una sola sesión, con su caché y sus locks, no cien usuarios.
- **Login como primer paso del flow:** cada iteración se loguea, así que se mide sobre todo el login. En shop-api, el login con `scrypt` sincrónico era justamente lo que rompía.

Lo real está en el medio: cada usuario entra una vez y después navega o compra muchas veces con su sesión.

## Decisión

### `vu_setup`

```yaml
data:
  users: { file: data/users.csv }
vu_setup:
  - name: login
    request:
      method: POST
      url: /auth/login
      json: { user: "{{users.user}}", password: "{{users.password}}" }
    extract:
      token: { json: $.access_token }
defaults:
  headers: { Authorization: "Bearer {{token}}" }
flows:
  buy:
    steps: [...]
```

- **Qué es:** pasos que **cada VU** corre una vez, en orden. Lo que extraen queda como variable de ese VU en todas sus iteraciones.
- **Qué ven:** las variables globales, lo extraído en `setup` y los datasets. Cada VU toma una fila de cada dataset que use su `vu_setup`. No heredan `defaults`, igual que `setup`, porque suelen correr antes de que exista lo que `defaults` usa (`{{token}}`).
- **El dataset es del VU:** si `vu_setup` usa `users`, los flows ven la fila de su VU y no toman otra. Así `{{users.user}}` en un check del flow es el mismo usuario que hizo login.
- **Restricciones:** no llevan `think`. Los nombres de paso son únicos en todo el escenario, como siempre.

### Cuándo corre: antes de la carga

Se preparan todos los VUs **antes** de arrancar la carga:

- con `constant_vus`, los `vus`;
- con `constant_rate` y `ramping_rate`, los `max_vus`, porque cada lugar de concurrencia es un VU con su sesión.

Se descartó prepararlos a demanda, en la primera iteración de cada VU. En el modelo abierto, esa iteración incluiría el login en su latencia, que se mide desde el instante planificado (ADR-003): los primeros segundos mostrarían una degradación que no es del sistema bajo carga. Prepararlos antes deja las métricas limpias, aunque con `max_vus` alto implica muchos logins. El resumen de `approve` lo dice: `login × 200 VUs = 200 requests antes de la carga`.

La preparación corre con a lo sumo 50 VUs a la vez y respeta `limits.max_rps`. La carga empieza cuando terminó. La duración de la carga no incluye la preparación.

### En el motor

En el modelo abierto, el semáforo de concurrencia pasa a ser un **pool de VUs**: una iteración toma un VU libre (con sus variables) y lo devuelve al terminar; si no hay, se descarta, igual que antes. En el modelo cerrado, cada VU es una tarea con sus variables. Sin `vu_setup`, los VUs no tienen variables y todo se comporta como antes.

### Fallas

- **Si falla en algunos VUs:** sin respuesta, status ≥ 400, `extract` o check fallido. Esos VUs quedan afuera, la carga corre con los demás y un aviso dice cuántos.
- **Si falla en todos:** la corrida se aborta con el primer motivo, como pasa con `setup`.
- **`lt check`:** corre `vu_setup` una vez, para un VU, y después los flows con sus variables.

### Firma y resultados

- **Firma:** el plan canónico incluye `vu_setup` solo si existe, así que las aprobaciones de antes siguen valiendo.
- **Resultados:** `results.json` tiene `vu_setup`, con los VUs preparados, los que fallaron, la duración y, por paso, requests, fallas, percentiles y motivos. No entra en las métricas de los pasos ni en el diagnóstico.
- **Dónde se ve:** el resumen de `run` y el reporte HTML muestran una sección aparte.

## Consecuencias

- **A favor:** sesiones reales por usuario, sin ensuciar las métricas de carga. Los checks pueden verificar que cada respuesta sea del usuario correcto.
- **En contra:** un `max_vus` generoso multiplica los logins previos. Un token que vence durante la carga se renueva con `vu_refresh` (ADR-011).
