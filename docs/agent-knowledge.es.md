# Indexación de Conocimiento del Agente

[English](agent-knowledge.md) · [中文](agent-knowledge.zh.md) · [繁體中文](agent-knowledge.zh-TW.md) · [日本語](agent-knowledge.ja.md) · [한국어](agent-knowledge.ko.md) · [Русский](agent-knowledge.ru.md) · [Français](agent-knowledge.fr.md) · [Deutsch](agent-knowledge.de.md) · [Português](agent-knowledge.pt.md) · **Español** · [العربية](agent-knowledge.ar.md) · [Italiano](agent-knowledge.it.md) · [Ελληνικά](agent-knowledge.el.md) · [ไทย](agent-knowledge.th.md) · [Bahasa Melayu](agent-knowledge.ms.md)

El historial de proyectos entre Agentes se maneja por separado como `agent_memory`; consulte [Indexación de Memoria entre Agentes](agent-memory.md).

## Tipos de Documentos

| Archivo | Tipo AWI | Metadatos estructurados |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | directorio de ámbito, profundidad de precedencia, títulos, referencias locales |
| `SKILL.md` | `agent_skill` | `name` y `description` YAML, títulos, referencias locales |

El texto Markdown completo y limitado permanece consultable. `workspace_inspect` devuelve los metadatos estructurados junto con los metadatos normales del archivo y el extracto de contenido.

## Descubrimiento

Una reconciliación normal de workspace indexa archivos `AGENTS.md` y `SKILL.md` bajo ese workspace, sujeto a `.gitignore`, `.awiignore` y exclusiones predeterminadas de AWI.

El instalador de primera ejecución descubre adicionalmente:

- archivos `AGENTS.md` en directorios ancestros sobre el workspace seleccionado;
- manifiestos `SKILL.md` bajo directorios de proyecto y usuario conocidos para clientes de Agente soportados.

Cada documento externo se registra como una raíz de archivo único. AWI no indexa un directorio home completo o el contenido completo de paquetes Skill globales. `--skip-agent-knowledge` desactiva este descubrimiento adicional.

Los enlaces Markdown locales de un documento del Agente se registran solo cuando su destino existe y permanece dentro de la raíz del documento. Los archivos referenciados son consultables cuando ya están cubiertos por una raíz de workspace; de lo contrario, la ruta de referencia se devuelve como metadatos para inspección explícita con otra herramienta de archivo.

## Ámbito y Clasificación

Pase `context_path` a `workspace_search` al resolver instrucciones de repositorio:

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI excluye archivos `AGENTS.md` cuyo ámbito no es un ancestro de la ruta de contexto. Los archivos aplicables se clasifican por profundidad de ámbito para que las instrucciones más cercanas vengan primero. Después de esta clasificación, los documentos del Agente con contenido idéntico se colapsan, lo que suprime duplicados de Skill y worktree copiados sin perder la instrucción aplicable más cercana.

Use `--kind agent_skill` para recuperación centrada en Skill. Los documentos del Agente se mantienen fuera de búsquedas comunes de código/datos; la vía dedicada del Agente se activa mediante un filtro de tipo de Agente o `context_path`.

## Seguridad

Los documentos del Agente permanecen sujetos a verificaciones normales de tamaño, UTF-8, enlace simbólico y nombre de archivo sensible. Los patrones de clave privada y token de proveedor de alta confianza en su contenido causan un resultado solo de metadatos con `metadata_only_sensitive_content`; el contenido, la vista previa y los metadatos del Agente analizados no se almacenan.

El frontmatter Skill malformado se registra como un fallo de extracción, mientras que el nuevo contenido Markdown aún reemplaza cualquier texto indexado más antiguo. Esto evita servir contenido obsoleto cuando solo falla el análisis de metadatos estructurados.

## Validación de Desarrollo

Una instalación aislada sobre el entorno Bona actual descubrió 136 documentos de Agente canónicos con cero fallos de extracción. Las consultas de humo dirigidas seleccionaron `wukong-dag-failure-debugger` primero para una consulta de fallo Wukong DAG/LogID y el `AGENTS.md` de Bona para un contexto de origen AWI.

En el conjunto de recuperación de desarrollo existente de 16 casos, agregar estos documentos de Agente mantuvo Recall@10 en `1.0` y tasa de resultados obsoletos en `0`; MRR cambió de `0.5975` a `0.6027`, nDCG@10 de `0.6922` a `0.6969`, y P95 de compilación de release de `8.07 ms` a `25.88 ms`. La latencia permanece por debajo del objetivo de búsqueda híbrida de `150 ms`. Este conjunto de desarrollo permanece pendiente de doble revisión y no es un benchmark de release congelado.
