# ADR-001 — Claude-first: CLI con salida JSON + skill y hook de Claude Code

Estado: **aceptado** · 12 oct 2026 · revisado 12 oct 2026 (forma de instalación)

## Contexto

La herramienta tiene que poder usarse con un agente que diseñe escenarios y analice resultados. El plan original apuntaba a un servidor MCP genérico desde el inicio, además de importadores (OpenAPI, curl, Postman) y una GUI.

Somos una sola persona y el cliente que vamos a usar es Claude Code, que ya tiene shell y lee y escribe archivos del repo.

## Decisión

1. **Un núcleo, interfaces delgadas.** Toda la lógica (escenario, motor, métricas, diagnóstico) vive en una librería. Las interfaces solo la llaman.
2. **La primera interfaz es la CLI `lt`**, y cada comando que Claude usa tiene salida `--json` estable (hoy `validate`, `check` y `run`; más adelante `findings`). `lt schema` imprime el JSON Schema del escenario, generado desde los tipos.
3. **La integración con Claude se instala con `lt claude install`** en el repo del usuario:
   - el *skill* `loadtest` en `.claude/skills/loadtest/SKILL.md` (fuente: `integrations/claude/`), que enseña el formato de escenario (ADR-002) y el ciclo diseñar → validar → `check` → la persona ejecuta → analizar. Se invoca como `/loadtest` o solo, cuando el pedido encaja;
   - en `.claude/settings.json`, un hook `PreToolUse` que llama a `lt hook pre-tool-use` y bloquea `lt run` y `lt approve` cuando los invoca Claude (defensa adicional; la guarda real está en el binario, ver ADR-004);
   - en el mismo archivo, permisos para que Claude use `lt validate`, `lt check` y `lt schema` sin pedir confirmación.

   Se descartó por ahora empaquetarlo como plugin de marketplace: los archivos de proyecto son el mecanismo más simple y estable, y no dependen del formato de plugins. Se revisa al publicar.
4. **No hay importadores en la primera versión.** Claude lee el OpenAPI, el curl o el código y escribe el YAML. Lo que hace falta es que `validate` dé errores precisos.
5. **El MCP queda para después**, como otra interfaz delgada sobre el mismo núcleo, cuando haga falta soportar otros clientes (Cursor, etc.).

## Consecuencias

- Hay menos que construir antes de que la herramienta sirva: motor + CLI + skill.
- La salida JSON de la CLI pasa a ser un contrato: se versiona igual que el escenario.
- Sin MCP, otros agentes pueden usar la CLI por shell, pero sin el skill.
- Si Claude escribe mal los escenarios con frecuencia, se mejora el skill y los mensajes de `validate` antes de pensar en importadores.
- `lt` tiene que estar en el PATH (`cargo install --path crates/lt-cli`): el skill y el hook lo invocan como `lt`. Si no está, el hook falla abierto; la guarda del binario sigue valiendo.
