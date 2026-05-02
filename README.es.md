<p align="center">
  <h1 align="center">Claudex</h1>
  <p align="center">Gestor multi-instancia de Claude Code con proxy de traducción inteligente</p>
</p>

<p align="center">
  <a href="https://github.com/pilc80/claudex/actions/workflows/ci.yml"><img src="https://github.com/pilc80/claudex/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/pilc80/claudex/releases"><img src="https://github.com/pilc80/claudex/actions/workflows/release.yml/badge.svg" alt="Release"></a>
  <a href="https://github.com/pilc80/claudex/blob/main/LICENSE"><img src="https://img.shields.io/github/license/pilc80/claudex" alt="Licencia"></a>
  <a href="https://github.com/pilc80/claudex/releases"><img src="https://img.shields.io/github/v/release/pilc80/claudex" alt="Última versión"></a>
</p>

<p align="center">
  <a href="https://github.com/pilc80/claudex">Documentación</a>
</p>

<p align="center">
  <a href="./README.md">English</a> |
  <a href="./README.zh-CN.md">简体中文</a> |
  <a href="./README.zh-TW.md">繁體中文</a> |
  <a href="./README.ja.md">日本語</a> |
  <a href="./README.ko.md">한국어</a> |
  <a href="./README.ru.md">Русский</a> |
  <a href="./README.fr.md">Français</a> |
  <a href="./README.pt-BR.md">Português do Brasil</a> |
  Español |
  <a href="./README.it.md">Italiano</a> |
  <a href="./README.de.md">Deutsch</a> |
  <a href="./README.pl.md">Polski</a>
</p>

---

Claudex es un proxy unificado que permite a [Claude Code](https://docs.anthropic.com/en/docs/claude-code) trabajar de forma transparente con múltiples proveedores de IA mediante traducción automática de protocolos.

## Características

- **Proxy multi-proveedor** — Reenvío directo DirectAnthropic + traducción Anthropic <-> OpenAI Chat Completions + traducción Anthropic <-> Responses API
- **Más de 20 proveedores** — Anthropic, OpenRouter, Grok, OpenAI, DeepSeek, Kimi, GLM, Groq, Mistral, Together AI, Perplexity, Cerebras, Azure OpenAI, Google Vertex AI, Ollama, LM Studio y más
- **Traducción en streaming** — Traducción completa de flujos SSE con soporte para llamadas a herramientas
- **Circuit breaker + conmutación por error** — Fallback automático a proveedores de respaldo con umbrales configurables
- **Enrutamiento inteligente** — Enrutamiento automático basado en intención mediante clasificador local
- **Motor de contexto** — Compresión de conversaciones, compartición entre perfiles, RAG local con embeddings
- **Suscripciones OAuth** — ChatGPT/Codex, Claude Max, GitHub Copilot, GitLab Duo, Google Gemini, Qwen, Kimi
- **Conjuntos de configuración** — Instala y gestiona conjuntos de configuración reutilizables de Claude Code desde repositorios git
- **Panel TUI** — Estado de perfiles en tiempo real, métricas, registros y lanzamiento rápido
- **Actualización automática** — `claudex-config update` descarga la última versión desde GitHub

## Instalación

```bash
# Una línea (Linux / macOS)
curl -fsSL https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash

# Desde el código fuente
cargo install --git https://github.com/pilc80/claudex

# O descarga desde GitHub Releases
# https://github.com/pilc80/claudex/releases
```

### Requisitos del sistema

- macOS (Intel / Apple Silicon) o Linux (x86_64 / ARM64)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) instalado
- Windows: descarga el binario precompilado desde [Releases](https://github.com/pilc80/claudex/releases)

## Inicio rápido

```bash
# 1. Inicializar la configuración
claudex-config config init

# 2. Agregar un perfil de proveedor de forma interactiva
claudex-config profile add

# 3. Probar la conectividad
claudex-config profile test all

# 4. Ejecutar Claude Code con un proveedor específico
CLAUDEX_PROFILE=grok claudex

# 5. O usar el enrutamiento inteligente para seleccionar automáticamente el mejor proveedor
CLAUDEX_PROFILE=auto claudex
```

## Cómo funciona

```
CLAUDEX_PROFILE=openrouter-claude claudex
    │
    ├── Iniciar proxy (si no está en ejecución) → 127.0.0.1:13456
    │
    └── exec claude con variables de entorno:
        ANTHROPIC_BASE_URL=http://127.0.0.1:13456/proxy/openrouter-claude
        ANTHROPIC_AUTH_TOKEN=claudex-passthrough
        ANTHROPIC_MODEL=anthropic/claude-sonnet-4
        ANTHROPIC_DEFAULT_HAIKU_MODEL=...
        ANTHROPIC_DEFAULT_SONNET_MODEL=...
        ANTHROPIC_DEFAULT_OPUS_MODEL=...
```

El proxy intercepta las solicitudes y gestiona la traducción de protocolos:

- **DirectAnthropic** (Anthropic, MiniMax, Vertex AI) → reenvío con las cabeceras correctas
- **OpenAICompatible** (Grok, OpenAI, DeepSeek, etc.) → Anthropic → OpenAI Chat Completions → traduce la respuesta de vuelta
- **OpenAIResponses** (suscripciones ChatGPT/Codex) → Anthropic → Responses API → traduce la respuesta de vuelta

## Compatibilidad de proveedores

| Proveedor | Tipo | Traducción | Autenticación | Modelo de ejemplo |
|-----------|------|------------|---------------|-------------------|
| Anthropic | DirectAnthropic | Ninguna | API Key | `claude-sonnet-4-20250514` |
| MiniMax | DirectAnthropic | Ninguna | API Key | `claude-sonnet-4-20250514` |
| OpenRouter | OpenAICompatible | Anthropic <-> OpenAI | API Key | `anthropic/claude-sonnet-4` |
| Grok (xAI) | OpenAICompatible | Anthropic <-> OpenAI | API Key | `grok-3-beta` |
| OpenAI | OpenAICompatible | Anthropic <-> OpenAI | API Key | `gpt-4o` |
| DeepSeek | OpenAICompatible | Anthropic <-> OpenAI | API Key | `deepseek-chat` |
| Kimi | OpenAICompatible | Anthropic <-> OpenAI | API Key | `kimi-k2-0905-preview` |
| GLM (Zhipu) | OpenAICompatible | Anthropic <-> OpenAI | API Key | `glm-4-plus` |
| Groq | OpenAICompatible | Anthropic <-> OpenAI | API Key | `llama-3.3-70b` |
| Mistral | OpenAICompatible | Anthropic <-> OpenAI | API Key | `mistral-large-latest` |
| Together AI | OpenAICompatible | Anthropic <-> OpenAI | API Key | `meta-llama/...` |
| Perplexity | OpenAICompatible | Anthropic <-> OpenAI | API Key | `sonar-pro` |
| Cerebras | OpenAICompatible | Anthropic <-> OpenAI | API Key | `llama-3.3-70b` |
| Azure OpenAI | OpenAICompatible | Anthropic <-> OpenAI | cabecera api-key | `gpt-4o` |
| Google Vertex AI | DirectAnthropic | Ninguna | Bearer (gcloud) | `claude-sonnet-4@...` |
| Ollama | OpenAICompatible | Anthropic <-> OpenAI | Ninguna | `qwen2.5:72b` |
| LM Studio | OpenAICompatible | Anthropic <-> OpenAI | Ninguna | modelo local |
| Suscripción ChatGPT/Codex | OpenAIResponses | Anthropic <-> Responses | OAuth (PKCE/Device) | `gpt-5.5` |
| Claude Max sub | DirectAnthropic | Ninguna | OAuth (archivo) | `claude-sonnet-4` |
| GitHub Copilot | OpenAICompatible | Anthropic <-> OpenAI | OAuth (Device+Bearer) | `gpt-4o` |
| GitLab Duo | OpenAICompatible | Anthropic <-> OpenAI | GITLAB_TOKEN | `claude-sonnet-4` |

## Configuración

Claudex busca archivos de configuración en este orden:

1. Variable de entorno `$CLAUDEX_CONFIG`
2. `./claudex.toml` o `./claudex.yaml` (directorio actual)
3. `./.claudex/config.toml`
4. Directorios padre (hasta 10 niveles)
5. `~/.config/claudex/config.toml` (global, recomendado)

Admite los formatos TOML y YAML. Consulta [`config.example.toml`](./config.example.toml) para la referencia completa.

## Referencia de CLI

| Comando | Descripción |
|---------|-------------|
| `CLAUDEX_PROFILE=<profile> claudex` | Ejecutar Claude Code con un proveedor específico |
| `CLAUDEX_PROFILE=auto claudex` | Enrutamiento inteligente — selección automática del mejor proveedor |
| `CLAUDEX_PROFILE=<profile> CLAUDEX_MODEL=<model> claudex` | Sobreescribir el modelo para una sesión |
| `claudex-config profile list` | Listar todos los perfiles configurados |
| `claudex-config profile add` | Asistente interactivo de configuración de perfiles |
| `claudex-config profile show <name>` | Mostrar detalles del perfil |
| `claudex-config profile remove <name>` | Eliminar un perfil |
| `claudex-config profile test <name\|all>` | Probar la conectividad del proveedor |
| `claudex-config proxy start [-p port] [-d]` | Iniciar el proxy (opcionalmente como demonio) |
| `claudex-config proxy stop` | Detener el demonio del proxy |
| `claudex-config proxy status` | Mostrar el estado del proxy |
| `claudex-config dashboard` | Lanzar el panel TUI |
| `claudex-config config show [--raw] [--json]` | Mostrar la configuración cargada |
| `claudex-config config init [--yaml]` | Crear configuración en el directorio actual |
| `claudex-config config edit [--global]` | Abrir la configuración en $EDITOR |
| `claudex-config config validate [--connectivity]` | Validar la configuración |
| `claudex-config config get <key>` | Obtener un valor de configuración |
| `claudex-config config set <key> <value>` | Establecer un valor de configuración |
| `claudex-config config export --format <fmt>` | Exportar la configuración (json/toml/yaml) |
| `claudex-config update [--check]` | Actualización automática desde GitHub Releases |
| `claudex-config auth login <provider>` | Inicio de sesión OAuth |
| `claudex-config auth login github --enterprise-url <domain>` | GitHub Enterprise Copilot |
| `claudex-config auth status` | Mostrar el estado del token OAuth |
| `claudex-config auth logout <profile>` | Eliminar el token OAuth |
| `claudex-config auth refresh <profile>` | Forzar la renovación del token OAuth |
| `claudex-config sets add <source> [--global]` | Instalar un conjunto de configuración |
| `claudex-config sets remove <name>` | Eliminar un conjunto de configuración |
| `claudex-config sets list [--global]` | Listar los conjuntos instalados |
| `claudex-config sets update [name]` | Actualizar los conjuntos a la última versión |

## Suscripciones OAuth

Usa suscripciones existentes en lugar de claves de API:

```bash
# Suscripción de ChatGPT (detecta automáticamente las credenciales del Codex CLI)
claudex-config auth login chatgpt --profile codex-sub

# ChatGPT forzar inicio de sesión en el navegador
claudex-config auth login chatgpt --profile codex-sub --force

# ChatGPT sin interfaz gráfica (SSH/sin navegador)
claudex-config auth login chatgpt --profile codex-sub --force --headless

# GitHub Copilot
claudex-config auth login github --profile copilot

# GitHub Copilot Enterprise
claudex-config auth login github --profile copilot-ent --enterprise-url company.ghe.com

# GitLab Duo (lee la variable de entorno GITLAB_TOKEN)
claudex-config auth login gitlab --profile gitlab-duo

# Comprobar estado
claudex-config auth status

# Ejecutar con suscripción
CLAUDEX_PROFILE=codex-sub claudex
```

Compatible con: `claude`, `chatgpt`/`openai`, `google`, `qwen`, `kimi`, `github`/`copilot`, `gitlab`

## Asignación de ranuras de modelos

Mapea el selector `/model` de Claude Code (haiku/sonnet/opus) a los modelos de cualquier proveedor:

```toml
[[profiles]]
name = "openrouter-deepseek"
provider_type = "OpenAICompatible"
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-..."
default_model = "deepseek/deepseek-chat-v3-0324"

[profiles.models]
haiku = "deepseek/deepseek-chat-v3-0324"
sonnet = "deepseek/deepseek-chat-v3-0324"
opus = "deepseek/deepseek-r1"
```

## Arquitectura

```
src/
├── lib.rs
├── bin/
│   ├── claudex.rs
│   └── claudex-config.rs
├── cli.rs
├── update.rs
├── util.rs
├── config/
│   ├── mod.rs          # Descubrimiento y análisis de configuración (figment)
│   ├── cmd.rs          # Subcomandos config get/set/export/validate
│   └── profile.rs      # CRUD de perfiles + prueba de conectividad
├── process/
│   ├── mod.rs
│   ├── launch.rs       # Lanzador del proceso de Claude
│   └── daemon.rs       # Archivo PID + gestión de procesos
├── oauth/
│   ├── mod.rs          # AuthType, OAuthProvider, OAuthToken
│   ├── source.rs       # Capa 1: fuentes de credenciales (env/archivo/keyring)
│   ├── exchange.rs     # Capa 2: intercambio de tokens (PKCE/device code/refresh)
│   ├── manager.rs      # Capa 3: caché + deduplicación concurrente + reintento 401
│   ├── handler.rs      # Trait OAuthProviderHandler
│   ├── providers.rs    # Lógica CLI de inicio de sesión/actualización/estado
│   ├── server.rs       # Servidor de callback OAuth + sondeo de device code
│   └── token.rs        # Re-exportaciones
├── proxy/
│   ├── mod.rs          # Servidor Axum + ProxyState
│   ├── handler.rs      # Enrutamiento de solicitudes + circuit breaker + reintento 401
│   ├── adapter/        # Adaptadores específicos por proveedor
│   │   ├── mod.rs      # Trait ProviderAdapter + fábrica
│   │   ├── direct.rs   # DirectAnthropic (reenvío directo)
│   │   ├── chat_completions.rs  # OpenAI Chat Completions
│   │   └── responses.rs         # OpenAI Responses API
│   ├── translate/      # Traducción de protocolos
│   │   ├── chat_completions.rs
│   │   ├── chat_completions_stream.rs
│   │   ├── responses.rs
│   │   └── responses_stream.rs
│   ├── context_engine.rs
│   ├── fallback.rs     # Circuit breaker
│   ├── health.rs
│   ├── metrics.rs
│   ├── models.rs
│   ├── error.rs
│   └── util.rs
├── router/
│   ├── mod.rs
│   └── classifier.rs
├── context/
│   ├── mod.rs
│   ├── compression.rs
│   ├── sharing.rs
│   └── rag.rs
├── sets/               # Gestión de conjuntos de configuración
│   ├── mod.rs
│   ├── schema.rs
│   ├── source.rs
│   ├── install.rs
│   ├── lock.rs
│   ├── conflict.rs
│   └── mcp.rs
├── terminal/           # Detección de terminal + hipervínculos
│   ├── mod.rs
│   ├── detect.rs
│   ├── osc8.rs
│   └── pty.rs
└── tui/
    ├── mod.rs
    ├── dashboard.rs
    ├── input.rs
    └── widgets.rs
```

## Licencia

[MIT](./LICENSE)
