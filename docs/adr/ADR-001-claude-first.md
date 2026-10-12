# ADR-001 — Claude-first: CLI con salida JSON + plugin de Claude Code

Estado: **aceptado** · 12 oct 2026

## Contexto

La herramienta tiene que poder usarse con un agente que diseñe escenarios y analice resultados. El plan original apuntaba a un servidor MCP genérico desde el inicio, además de importadores (OpenAPI, curl, Postman) y una GUI.

Somos una sola persona y el cliente que vamos a usar es Claude Code, que ya tiene shell y lee y escribe archivos del repo.

## Decisión

1. **Un núcleo, interfaces delgadas.** Toda la lógica (escenario, motor, métricas, diagnóstico) vive en una librería. Las interfaces solo la llaman.
2. **La primera interfaz es la CLI `lt`**, y cada comando que Claude usa tiene salida `--json` estable: `validate`, `check`, `runs`, `results` y `findings`.
3. **La integración con Claude es un plugin de Claude Code**, que contiene:
   - un *skill* que enseña el formato de escenario (ADR-002) y el ciclo diseñar → validar → `check` → esperar aprobación → diagnosticar;
   - los comandos `/loadtest:design` y `/loadtest:diagnose`;
   - un hook `PreToolUse` que bloquea `lt approve` y `lt run` cuando los invoca Claude (defensa adicional; la guarda real está en el binario, ver ADR-004).
4. **No hay importadores en la primera versión.** Claude lee el OpenAPI, el curl o el código y escribe el YAML. Lo que hace falta es que `validate` dé errores precisos.
5. **El MCP queda para después**, como otra interfaz delgada sobre el mismo núcleo, cuando haga falta soportar otros clientes (Cursor, etc.).

## Consecuencias

- Hay menos que construir antes de que la herramienta sirva: motor + CLI + skill.
- La salida JSON de la CLI pasa a ser un contrato: se versiona igual que el escenario.
- Sin MCP, otros agentes pueden usar la CLI por shell, pero sin el skill.
- Si Claude escribe mal los escenarios con frecuencia, se mejora el skill y los mensajes de `validate` antes de pensar en importadores.
