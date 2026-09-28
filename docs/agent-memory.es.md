# Indexación de Memoria entre Agentes

[English](agent-memory.md) · [中文](agent-memory.zh.md) · [繁體中文](agent-memory.zh-TW.md) · [日本語](agent-memory.ja.md) · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · [Deutsch](agent-memory.de.md) · [Português](agent-memory.pt.md) · **Español** · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## Uso

```bash
# Solo memoria con curaduría y resúmenes.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# Incluir explícitamente historial bruto de chat/sesión coincidente.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# Buscar solo memoria asociada al proyecto actual.
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

El instalador ejecuta la forma con curaduría por defecto. Use `--skip-agent-memory` para desactivarla o `--include-raw-memory` para optar por el historial bruto.

## Fuentes

El descubrimiento está restringido al proyecto canónico seleccionado:

| Agente | Fuentes con curaduría predeterminadas | Opt-in bruto |
|---|---|---|
| Trae | perfil de usuario, memoria de proyecto coincidente, resúmenes de temas, resúmenes de sesiones | nada más allá de esos resúmenes |
| Codex | `MEMORY.md`, `memory_summary.md`, resúmenes de rollout coincidentes | JSONL de rollout bruto vinculado y `raw_memories.md` |
| Zcode | raíz de memoria mapeada al proyecto por los metadatos de runtime de Zcode | archivos de rollout/agente coincidentes |
| Gemini CLI | ninguna | historial de proyecto mapeado y directorios de chat |
| Claude Code | directorio de memoria de proyecto coincidente | archivos de sesión que identifican el proyecto |

Los directorios de sesión Argos/SRE están intencionalmente excluidos porque requieren el flujo de trabajo de diagnóstico Argos en lugar de indexación masiva de archivos. AWI nunca escanea todo el directorio home.

## Metadatos y Clasificación

Cada resultado de memoria incluye:

- Agente de origen;
- capa: `user_profile`, `project_summary`, `topic_summary`, `session_summary`, `memory_note` o `raw_history`;
- `name` y `description` YAML frontmatter opcionales;
- raíz de workspace canónico y clave de proyecto del proveedor cuando están disponibles;
- ID de sesión cuando se puede derivar;
- tiempo de observación y flag de historial bruto.

Cuando se proporciona `context_path` o un filtro de raíz de proyecto, la memoria de otro proyecto es rechazada. La memoria del proyecto exacto se clasifica antes que la memoria global. Las coincidencias exactas de nombre de archivo y `name` frontmatter reciben el mayor impulso de metadatos; la superposición de descripción proporciona un impulso menor. Las consultas en forma de identificador degradan los resúmenes amplios de `MEMORY.md`/proyecto a menos que el resumen en sí coincida exactamente con la entidad. El orden normal de capas permanece sin cambios para consultas amplias, los resúmenes con curaduría se clasifican antes que el historial bruto, y la recencia es solo un pequeño desempate. Las copias con contenido idéntico de diferentes Agentes se colapsan después de la clasificación. La recuperación de memoria se activa con `--kind agent_memory`. Los documentos de memoria usan un índice Tantivy dedicado para que su vocabulario no pueda cambiar las estadísticas IDF o la clasificación común de código/datos.

## Análisis y Seguridad

La memoria Markdown se indexa como texto limitado. La memoria JSON/JSONL se normaliza a campos relevantes para humanos como intención, acciones, resultado, hechos aprendidos, rol, mensaje y contenido; los IDs de transporte y metadatos de resumen internos se omiten del documento de búsqueda. La memoria JSONL no se envía a perfilado DuckDB.

Las verificaciones normales de tamaño, UTF-8, ignorar y nombre de archivo sensible aún se aplican. Las credenciales o claves privadas de alta confianza hacen que el archivo de memoria completo sea solo metadatos. El historial bruto está desactivado a menos que se solicite explícitamente, y los archivos brutos superdimensionados permanecen solo metadatos bajo el límite de contenido configurado.

`awi memory` persiste la raíz del proyecto canónico y la política de historial bruto. Cada ciclo de productor redescubre las fuentes de Agente de ese proyecto antes de publicar, por lo que nuevos directorios de proyecto de proveedor aparecen sin reinstalar AWI. Las raíces registradas anteriormente también se reconcilian cuando desaparecen, evitando que la memoria eliminada permanezca consultable. El JSONL de resumen anexado se actualiza en la próxima reconciliación periódica; los archivos brutos pesados de anexación no activan reconstrucciones de instantánea por escritura.

`awi status --json` expone un objeto `memory` con recuentos de proyecto/fuente registrados, archivos activos, la generación de memoria más reciente y el retraso de generación relativo, `stale_files`/`missing_files` del sistema de archivos, retraso máximo de fuente y edad de la fuente más antigua. Los recuentos de archivos obsoletos y ausentes del sistema de archivos son las señales de frescura autoritativas; un bajo retraso de generación por sí solo no prueba que la memoria externa esté actualizada.

## Validación de Desarrollo

Una ejecución Bona aislada descubrió 35 raíces relevantes al proyecto e indexó 489 archivos con curaduría de Codex, Trae y Zcode con cero fallos de extracción; los chats brutos de Gemini permanecieron excluidos. El conjunto existente de 16 casos de recuperación de código/datos permaneció estable bit a bit en Recall@10 `1.0`, MRR `0.6006`, nDCG@10 `0.6950` y tasa de obsolescencia `0`. El P95 de compilación de release fue `24.06 ms`. Este conjunto sigue siendo `candidate_pending_dual_review`, no una puerta de release congelada. No informe precisión/recall formal, Inspect@K o MRR hasta que dos revisores distintos hayan etiquetado independientemente el conjunto de consultas y arbitrado las divergencias.
