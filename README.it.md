<p align="center">
  <h1 align="center">Claudex</h1>
  <p align="center">Gestore multi-istanza di Claude Code con proxy di traduzione intelligente</p>
</p>

<p align="center">
  <a href="https://github.com/pilc80/claudex/actions/workflows/ci.yml"><img src="https://github.com/pilc80/claudex/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/pilc80/claudex/releases"><img src="https://github.com/pilc80/claudex/actions/workflows/release.yml/badge.svg" alt="Release"></a>
  <a href="https://github.com/pilc80/claudex/blob/main/LICENSE"><img src="https://img.shields.io/github/license/pilc80/claudex" alt="License"></a>
  <a href="https://github.com/pilc80/claudex/releases"><img src="https://img.shields.io/github/v/release/pilc80/claudex" alt="Latest Release"></a>
</p>

<p align="center">
  <a href="https://github.com/pilc80/claudex">Documentazione</a>
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
  <a href="./README.es.md">Español</a> |
  Italiano |
  <a href="./README.de.md">Deutsch</a> |
  <a href="./README.pl.md">Polski</a>
</p>

---

Claudex è un proxy unificato che consente a [Claude Code](https://docs.anthropic.com/en/docs/claude-code) di lavorare senza interruzioni con più provider AI attraverso la traduzione automatica dei protocolli.

## Funzionalità

- **Proxy multi-provider** — Passthrough DirectAnthropic + traduzione Anthropic <-> OpenAI Chat Completions + traduzione Anthropic <-> Responses API
- **Oltre 20 provider** — Anthropic, OpenRouter, Grok, OpenAI, DeepSeek, Kimi, GLM, Groq, Mistral, Together AI, Perplexity, Cerebras, Azure OpenAI, Google Vertex AI, Ollama, LM Studio e altri
- **Traduzione streaming** — Traduzione completa del flusso SSE con supporto alle chiamate di strumenti
- **Circuit breaker + failover** — Fallback automatico ai provider di backup con soglie configurabili
- **Routing intelligente** — Routing automatico basato sulle intenzioni tramite classificatore locale
- **Motore contestuale** — Compressione delle conversazioni, condivisione cross-profile, RAG locale con embedding
- **Sottoscrizioni OAuth** — ChatGPT/Codex, Claude Max, GitHub Copilot, GitLab Duo, Google Gemini, Qwen, Kimi
- **Set di configurazione** — Installa e gestisce set di configurazione riutilizzabili di Claude Code da repository git
- **Dashboard TUI** — Stato dei profili in tempo reale, metriche, log e avvio rapido
- **Auto-aggiornamento** — `claudex-config update` scarica l'ultima versione da GitHub

## Installazione

```bash
# Installazione in un comando (Linux / macOS)
curl -fL --progress-bar https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash

# Dal sorgente
cargo install --git https://github.com/pilc80/claudex

# Oppure scarica da GitHub Releases
# https://github.com/pilc80/claudex/releases
```

### Requisiti di sistema

- macOS (Intel / Apple Silicon) o Linux (x86_64 / ARM64)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) installato
- Windows: scarica il binario precompilato da [Releases](https://github.com/pilc80/claudex/releases)

## Avvio rapido

```bash
# 1. Inizializza la configurazione
claudex-config config init

# 2. Aggiungi un profilo provider in modo interattivo
claudex-config profile add

# 3. Testa la connettività
claudex-config profile test all

# 4. Esegui Claude Code con un provider specifico
CLAUDEX_PROFILE=grok claudex

# 5. Oppure usa il routing intelligente per selezionare automaticamente il provider migliore
CLAUDEX_PROFILE=auto claudex
```

## Come funziona

```
CLAUDEX_PROFILE=openrouter-claude claudex
    │
    ├── Avvia il proxy (se non in esecuzione) → 127.0.0.1:13456
    │
    └── esegue claude con le variabili d'ambiente:
        ANTHROPIC_BASE_URL=http://127.0.0.1:13456/proxy/openrouter-claude
        ANTHROPIC_AUTH_TOKEN=claudex-passthrough
        ANTHROPIC_MODEL=anthropic/claude-sonnet-4
        ANTHROPIC_DEFAULT_HAIKU_MODEL=...
        ANTHROPIC_DEFAULT_SONNET_MODEL=...
        ANTHROPIC_DEFAULT_OPUS_MODEL=...
```

Il proxy intercetta le richieste e gestisce la traduzione dei protocolli:

- **DirectAnthropic** (Anthropic, MiniMax, Vertex AI) → inoltra con intestazioni corrette
- **OpenAICompatible** (Grok, OpenAI, DeepSeek, ecc.) → Anthropic → OpenAI Chat Completions → traduce la risposta
- **OpenAIResponses** (sottoscrizioni ChatGPT/Codex) → Anthropic → Responses API → traduce la risposta

## Compatibilità con i provider

| Provider | Tipo | Traduzione | Autenticazione | Modello di esempio |
|----------|------|------------|----------------|-------------------|
| Anthropic | DirectAnthropic | Nessuna | API Key | `claude-sonnet-4-20250514` |
| MiniMax | DirectAnthropic | Nessuna | API Key | `claude-sonnet-4-20250514` |
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
| Azure OpenAI | OpenAICompatible | Anthropic <-> OpenAI | intestazione api-key | `gpt-4o` |
| Google Vertex AI | DirectAnthropic | Nessuna | Bearer (gcloud) | `claude-sonnet-4@...` |
| Ollama | OpenAICompatible | Anthropic <-> OpenAI | Nessuna | `qwen2.5:72b` |
| LM Studio | OpenAICompatible | Anthropic <-> OpenAI | Nessuna | modello locale |
| Sottoscrizione ChatGPT/Codex | OpenAIResponses | Anthropic <-> Responses | OAuth (PKCE/Device) | `gpt-5.5` |
| Sottoscrizione Claude Max | DirectAnthropic | Nessuna | OAuth (file) | `claude-sonnet-4` |
| GitHub Copilot | OpenAICompatible | Anthropic <-> OpenAI | OAuth (Device+Bearer) | `gpt-4o` |
| GitLab Duo | OpenAICompatible | Anthropic <-> OpenAI | GITLAB_TOKEN | `claude-sonnet-4` |

## Configurazione

Claudex cerca i file di configurazione nel seguente ordine:

1. Variabile d'ambiente `$CLAUDEX_CONFIG`
2. `./claudex.toml` o `./claudex.yaml` (directory corrente)
3. `./.claudex/config.toml`
4. Directory superiori (fino a 10 livelli)
5. `~/.config/claudex/config.toml` (globale, consigliato)

Supporta i formati TOML e YAML. Consulta [`config.example.toml`](./config.example.toml) per il riferimento completo.

## Riferimento CLI

| Comando | Descrizione |
|---------|-------------|
| `CLAUDEX_PROFILE=<profile> claudex` | Esegui Claude Code con un provider specifico |
| `CLAUDEX_PROFILE=auto claudex` | Routing intelligente — seleziona automaticamente il provider migliore |
| `CLAUDEX_PROFILE=<profile> CLAUDEX_MODEL=<model> claudex` | Sovrascrive il modello per una sessione |
| `claudex-config profile list` | Elenca tutti i profili configurati |
| `claudex-config profile add` | Procedura guidata interattiva per la configurazione del profilo |
| `claudex-config profile show <name>` | Mostra i dettagli del profilo |
| `claudex-config profile remove <name>` | Rimuove un profilo |
| `claudex-config profile test <name\|all>` | Testa la connettività del provider |
| `claudex-config proxy start [-p port] [-d]` | Avvia il proxy (opzionalmente come daemon) |
| `claudex-config proxy stop` | Ferma il daemon del proxy |
| `claudex-config proxy status` | Mostra lo stato del proxy |
| `claudex-config dashboard` | Avvia la dashboard TUI |
| `claudex-config config show [--raw] [--json]` | Mostra la configurazione caricata |
| `claudex-config config init [--yaml]` | Crea la configurazione nella directory corrente |
| `claudex-config config edit [--global]` | Apre la configurazione in $EDITOR |
| `claudex-config config validate [--connectivity]` | Valida la configurazione |
| `claudex-config config get <key>` | Ottieni un valore di configurazione |
| `claudex-config config set <key> <value>` | Imposta un valore di configurazione |
| `claudex-config config export --format <fmt>` | Esporta la configurazione (json/toml/yaml) |
| `claudex-config update [--check]` | Auto-aggiornamento da GitHub Releases |
| `claudex-config auth login <provider>` | Accesso OAuth |
| `claudex-config auth login github --enterprise-url <domain>` | GitHub Enterprise Copilot |
| `claudex-config auth status` | Mostra lo stato del token OAuth |
| `claudex-config auth logout <profile>` | Rimuove il token OAuth |
| `claudex-config auth refresh <profile>` | Forza il rinnovo del token OAuth |
| `claudex-config sets add <source> [--global]` | Installa un set di configurazione |
| `claudex-config sets remove <name>` | Rimuove un set di configurazione |
| `claudex-config sets list [--global]` | Elenca i set installati |
| `claudex-config sets update [name]` | Aggiorna i set all'ultima versione |

## Sottoscrizioni OAuth

Usa le sottoscrizioni esistenti al posto delle API key:

```bash
# Sottoscrizione ChatGPT (rileva automaticamente le credenziali Codex CLI esistenti)
claudex-config auth login chatgpt --profile codex-sub

# ChatGPT con accesso forzato tramite browser
claudex-config auth login chatgpt --profile codex-sub --force

# ChatGPT in modalità headless (SSH/senza browser)
claudex-config auth login chatgpt --profile codex-sub --force --headless

# GitHub Copilot
claudex-config auth login github --profile copilot

# GitHub Copilot Enterprise
claudex-config auth login github --profile copilot-ent --enterprise-url company.ghe.com

# GitLab Duo (legge la variabile d'ambiente GITLAB_TOKEN)
claudex-config auth login gitlab --profile gitlab-duo

# Controlla lo stato
claudex-config auth status

# Esegui con la sottoscrizione
CLAUDEX_PROFILE=codex-sub claudex
```

Supportati: `claude`, `chatgpt`/`openai`, `google`, `qwen`, `kimi`, `github`/`copilot`, `gitlab`

## Mappatura degli slot del modello

Mappa il selettore `/model` di Claude Code (haiku/sonnet/opus) ai modelli di qualsiasi provider:

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

## Architettura

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
│   ├── mod.rs          # Rilevamento e analisi della configurazione (figment)
│   ├── cmd.rs          # Sottocomandi config get/set/export/validate
│   └── profile.rs      # CRUD dei profili + test di connettività
├── process/
│   ├── mod.rs
│   ├── launch.rs       # Avvio del processo Claude
│   └── daemon.rs       # File PID + gestione dei processi
├── oauth/
│   ├── mod.rs          # AuthType, OAuthProvider, OAuthToken
│   ├── source.rs       # Livello 1: sorgenti delle credenziali (env/file/keyring)
│   ├── exchange.rs     # Livello 2: scambio di token (PKCE/device code/refresh)
│   ├── manager.rs      # Livello 3: cache + deduplicazione concorrente + retry 401
│   ├── handler.rs      # Trait OAuthProviderHandler
│   ├── providers.rs    # Logica CLI di login/refresh/status
│   ├── server.rs       # Server di callback OAuth + polling device code
│   └── token.rs        # Re-export
├── proxy/
│   ├── mod.rs          # Server Axum + ProxyState
│   ├── handler.rs      # Routing delle richieste + circuit breaker + retry 401
│   ├── adapter/        # Adapter specifici per provider
│   │   ├── mod.rs      # Trait ProviderAdapter + factory
│   │   ├── direct.rs   # DirectAnthropic (passthrough)
│   │   ├── chat_completions.rs  # OpenAI Chat Completions
│   │   └── responses.rs         # OpenAI Responses API
│   ├── translate/      # Traduzione dei protocolli
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
├── sets/               # Gestione dei set di configurazione
│   ├── mod.rs
│   ├── schema.rs
│   ├── source.rs
│   ├── install.rs
│   ├── lock.rs
│   ├── conflict.rs
│   └── mcp.rs
├── terminal/           # Rilevamento del terminale + hyperlink
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

## Licenza

[MIT](./LICENSE)
