<p align="center">
  <h1 align="center">Claudex</h1>
  <p align="center">Gerenciador multi-instância do Claude Code com proxy de tradução inteligente</p>
</p>

<p align="center">
  <a href="https://github.com/pilc80/claudex/actions/workflows/ci.yml"><img src="https://github.com/pilc80/claudex/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/pilc80/claudex/releases"><img src="https://github.com/pilc80/claudex/actions/workflows/release.yml/badge.svg" alt="Release"></a>
  <a href="https://github.com/pilc80/claudex/blob/main/LICENSE"><img src="https://img.shields.io/github/license/pilc80/claudex" alt="License"></a>
  <a href="https://github.com/pilc80/claudex/releases"><img src="https://img.shields.io/github/v/release/pilc80/claudex" alt="Latest Release"></a>
</p>

<p align="center">
  <a href="https://github.com/pilc80/claudex">Documentação</a>
</p>

<p align="center">
  <a href="./README.md">English</a> |
  <a href="./README.zh-CN.md">简体中文</a> |
  <a href="./README.zh-TW.md">繁體中文</a> |
  <a href="./README.ja.md">日本語</a> |
  <a href="./README.ko.md">한국어</a> |
  <a href="./README.ru.md">Русский</a> |
  <a href="./README.fr.md">Français</a> |
  Português do Brasil |
  <a href="./README.es.md">Español</a> |
  <a href="./README.it.md">Italiano</a> |
  <a href="./README.de.md">Deutsch</a> |
  <a href="./README.pl.md">Polski</a>
</p>

---

Claudex é um proxy unificado que permite ao [Claude Code](https://docs.anthropic.com/en/docs/claude-code) trabalhar de forma transparente com múltiplos provedores de IA por meio de tradução automática de protocolos.

## Funcionalidades

- **Proxy multi-provedor** — Passthrough DirectAnthropic + tradução Anthropic <-> OpenAI Chat Completions + tradução Anthropic <-> Responses API
- **Mais de 20 provedores** — Anthropic, OpenRouter, Grok, OpenAI, DeepSeek, Kimi, GLM, Groq, Mistral, Together AI, Perplexity, Cerebras, Azure OpenAI, Google Vertex AI, Ollama, LM Studio e mais
- **Tradução de streaming** — Tradução completa de stream SSE com suporte a chamadas de ferramentas
- **Circuit breaker + failover** — Fallback automático para provedores de backup com thresholds configuráveis
- **Roteamento inteligente** — Roteamento automático baseado em intenção via classificador local
- **Motor de contexto** — Compressão de conversa, compartilhamento entre perfis, RAG local com embeddings
- **Assinaturas OAuth** — ChatGPT/Codex, Claude Max, GitHub Copilot, GitLab Duo, Google Gemini, Qwen, Kimi
- **Conjuntos de configuração** — Instale e gerencie conjuntos reutilizáveis de configuração do Claude Code a partir de repositórios git
- **Dashboard TUI** — Saúde dos perfis em tempo real, métricas, logs e inicialização rápida
- **Auto-atualização** — `claudex-config update` baixa a versão mais recente do GitHub

## Instalação

```bash
# Instalação em uma linha (Linux / macOS)
curl -fL --progress-bar https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash

# A partir do código-fonte
cargo install --git https://github.com/pilc80/claudex

# Ou baixe diretamente do GitHub Releases
# https://github.com/pilc80/claudex/releases
```

### Requisitos do Sistema

- macOS (Intel / Apple Silicon) ou Linux (x86_64 / ARM64)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) instalado
- Windows: baixe o binário pré-compilado em [Releases](https://github.com/pilc80/claudex/releases)

## Início Rápido

```bash
# 1. Inicializar configuração
claudex-config config init

# 2. Adicionar um perfil de provedor interativamente
claudex-config profile add

# 3. Testar conectividade
claudex-config profile test all

# 4. Executar o Claude Code com um provedor específico
CLAUDEX_PROFILE=grok claudex

# 5. Ou usar roteamento inteligente para selecionar automaticamente o melhor provedor
CLAUDEX_PROFILE=auto claudex
```

## Como Funciona

```
CLAUDEX_PROFILE=openrouter-claude claudex
    │
    ├── Inicia proxy (se não estiver em execução) → 127.0.0.1:13456
    │
    └── executa claude com variáveis de ambiente:
        ANTHROPIC_BASE_URL=http://127.0.0.1:13456/proxy/openrouter-claude
        ANTHROPIC_AUTH_TOKEN=claudex-passthrough
        ANTHROPIC_MODEL=anthropic/claude-sonnet-4
        ANTHROPIC_DEFAULT_HAIKU_MODEL=...
        ANTHROPIC_DEFAULT_SONNET_MODEL=...
        ANTHROPIC_DEFAULT_OPUS_MODEL=...
```

O proxy intercepta as requisições e realiza a tradução de protocolo:

- **DirectAnthropic** (Anthropic, MiniMax, Vertex AI) → encaminha com os headers corretos
- **OpenAICompatible** (Grok, OpenAI, DeepSeek, etc.) → Anthropic → OpenAI Chat Completions → traduz resposta de volta
- **OpenAIResponses** (assinaturas ChatGPT/Codex) → Anthropic → Responses API → traduz resposta de volta

## Compatibilidade de Provedores

| Provedor | Tipo | Tradução | Autenticação | Modelo de Exemplo |
|----------|------|----------|--------------|-------------------|
| Anthropic | DirectAnthropic | Nenhuma | API Key | `claude-sonnet-4-20250514` |
| MiniMax | DirectAnthropic | Nenhuma | API Key | `claude-sonnet-4-20250514` |
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
| Azure OpenAI | OpenAICompatible | Anthropic <-> OpenAI | header api-key | `gpt-4o` |
| Google Vertex AI | DirectAnthropic | Nenhuma | Bearer (gcloud) | `claude-sonnet-4@...` |
| Ollama | OpenAICompatible | Anthropic <-> OpenAI | Nenhuma | `qwen2.5:72b` |
| LM Studio | OpenAICompatible | Anthropic <-> OpenAI | Nenhuma | modelo local |
| Assinatura ChatGPT/Codex | OpenAIResponses | Anthropic <-> Responses | OAuth (PKCE/Device) | `gpt-5.5` |
| Assinatura Claude Max | DirectAnthropic | Nenhuma | OAuth (arquivo) | `claude-sonnet-4` |
| GitHub Copilot | OpenAICompatible | Anthropic <-> OpenAI | OAuth (Device+Bearer) | `gpt-4o` |
| GitLab Duo | OpenAICompatible | Anthropic <-> OpenAI | GITLAB_TOKEN | `claude-sonnet-4` |

## Configuração

O Claudex busca arquivos de configuração na seguinte ordem:

1. Variável de ambiente `$CLAUDEX_CONFIG`
2. `./claudex.toml` ou `./claudex.yaml` (diretório atual)
3. `./.claudex/config.toml`
4. Diretórios pai (até 10 níveis)
5. `~/.config/claudex/config.toml` (global, recomendado)

Suporta os formatos TOML e YAML. Consulte [`config.example.toml`](./config.example.toml) para a referência completa.

## Referência de Comandos CLI

| Comando | Descrição |
|---------|-----------|
| `CLAUDEX_PROFILE=<profile> claudex` | Executar o Claude Code com um provedor específico |
| `CLAUDEX_PROFILE=auto claudex` | Roteamento inteligente — seleciona automaticamente o melhor provedor |
| `CLAUDEX_PROFILE=<profile> CLAUDEX_MODEL=<model> claudex` | Sobrescrever o modelo para uma sessão |
| `claudex-config profile list` | Listar todos os perfis configurados |
| `claudex-config profile add` | Assistente interativo de configuração de perfil |
| `claudex-config profile show <name>` | Exibir detalhes do perfil |
| `claudex-config profile remove <name>` | Remover um perfil |
| `claudex-config profile test <name\|all>` | Testar conectividade do provedor |
| `claudex-config proxy start [-p port] [-d]` | Iniciar proxy (opcionalmente como daemon) |
| `claudex-config proxy stop` | Parar daemon do proxy |
| `claudex-config proxy status` | Exibir status do proxy |
| `claudex-config dashboard` | Abrir dashboard TUI |
| `claudex-config config show [--raw] [--json]` | Exibir configuração carregada |
| `claudex-config config init [--yaml]` | Criar configuração no diretório atual |
| `claudex-config config edit [--global]` | Abrir configuração no $EDITOR |
| `claudex-config config validate [--connectivity]` | Validar configuração |
| `claudex-config config get <key>` | Obter um valor de configuração |
| `claudex-config config set <key> <value>` | Definir um valor de configuração |
| `claudex-config config export --format <fmt>` | Exportar configuração (json/toml/yaml) |
| `claudex-config update [--check]` | Auto-atualização a partir do GitHub Releases |
| `claudex-config auth login <provider>` | Login OAuth |
| `claudex-config auth login github --enterprise-url <domain>` | GitHub Enterprise Copilot |
| `claudex-config auth status` | Exibir status do token OAuth |
| `claudex-config auth logout <profile>` | Remover token OAuth |
| `claudex-config auth refresh <profile>` | Forçar renovação do token OAuth |
| `claudex-config sets add <source> [--global]` | Instalar um conjunto de configuração |
| `claudex-config sets remove <name>` | Remover um conjunto de configuração |
| `claudex-config sets list [--global]` | Listar conjuntos instalados |
| `claudex-config sets update [name]` | Atualizar conjuntos para a versão mais recente |

## Assinaturas OAuth

Use assinaturas existentes em vez de chaves de API:

```bash
# Assinatura ChatGPT (detecta automaticamente credenciais existentes do Codex CLI)
claudex-config auth login chatgpt --profile codex-sub

# ChatGPT com login forçado pelo navegador
claudex-config auth login chatgpt --profile codex-sub --force

# ChatGPT sem interface gráfica (SSH/sem navegador)
claudex-config auth login chatgpt --profile codex-sub --force --headless

# GitHub Copilot
claudex-config auth login github --profile copilot

# GitHub Copilot Enterprise
claudex-config auth login github --profile copilot-ent --enterprise-url company.ghe.com

# GitLab Duo (lê a variável de ambiente GITLAB_TOKEN)
claudex-config auth login gitlab --profile gitlab-duo

# Verificar status
claudex-config auth status

# Executar com assinatura
CLAUDEX_PROFILE=codex-sub claudex
```

Suportados: `claude`, `chatgpt`/`openai`, `google`, `qwen`, `kimi`, `github`/`copilot`, `gitlab`

## Mapeamento de Slots de Modelo

Mapeie o seletor `/model` do Claude Code (haiku/sonnet/opus) para os modelos de qualquer provedor:

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

## Arquitetura

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
│   ├── mod.rs          # Descoberta e parsing de configuração (figment)
│   ├── cmd.rs          # Subcomandos config get/set/export/validate
│   └── profile.rs      # CRUD de perfil + teste de conectividade
├── process/
│   ├── mod.rs
│   ├── launch.rs       # Inicializador do processo Claude
│   └── daemon.rs       # Arquivo PID + gerenciamento de processos
├── oauth/
│   ├── mod.rs          # AuthType, OAuthProvider, OAuthToken
│   ├── source.rs       # Camada 1: fontes de credenciais (env/arquivo/keyring)
│   ├── exchange.rs     # Camada 2: troca de tokens (PKCE/device code/refresh)
│   ├── manager.rs      # Camada 3: cache + deduplicação concorrente + retry 401
│   ├── handler.rs      # trait OAuthProviderHandler
│   ├── providers.rs    # Lógica CLI de login/refresh/status
│   ├── server.rs       # Servidor de callback OAuth + polling de device code
│   └── token.rs        # Re-exports
├── proxy/
│   ├── mod.rs          # Servidor Axum + ProxyState
│   ├── handler.rs      # Roteamento de requisições + circuit breaker + retry 401
│   ├── adapter/        # Adaptadores específicos por provedor
│   │   ├── mod.rs      # trait ProviderAdapter + factory
│   │   ├── direct.rs   # DirectAnthropic (passthrough)
│   │   ├── chat_completions.rs  # OpenAI Chat Completions
│   │   └── responses.rs         # OpenAI Responses API
│   ├── translate/      # Tradução de protocolo
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
├── sets/               # Gerenciamento de conjuntos de configuração
│   ├── mod.rs
│   ├── schema.rs
│   ├── source.rs
│   ├── install.rs
│   ├── lock.rs
│   ├── conflict.rs
│   └── mcp.rs
├── terminal/           # Detecção de terminal + hyperlinks
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

## Licença

[MIT](./LICENSE)
