<p align="center">
  <h1 align="center">Claudex</h1>
  <p align="center">Gestionnaire multi-instances Claude Code avec proxy de traduction intelligent</p>
</p>

<p align="center">
  <a href="https://github.com/pilc80/claudex/actions/workflows/ci.yml"><img src="https://github.com/pilc80/claudex/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/pilc80/claudex/releases"><img src="https://github.com/pilc80/claudex/actions/workflows/release.yml/badge.svg" alt="Release"></a>
  <a href="https://github.com/pilc80/claudex/blob/main/LICENSE"><img src="https://img.shields.io/github/license/pilc80/claudex" alt="License"></a>
  <a href="https://github.com/pilc80/claudex/releases"><img src="https://img.shields.io/github/v/release/pilc80/claudex" alt="Latest Release"></a>
</p>

<p align="center">
  <a href="https://github.com/pilc80/claudex">Documentation</a>
</p>

<p align="center">
  <a href="./README.md">English</a> |
  <a href="./README.zh-CN.md">简体中文</a> |
  <a href="./README.zh-TW.md">繁體中文</a> |
  <a href="./README.ja.md">日本語</a> |
  <a href="./README.ko.md">한국어</a> |
  <a href="./README.ru.md">Русский</a> |
  Français |
  <a href="./README.pt-BR.md">Português do Brasil</a> |
  <a href="./README.es.md">Español</a> |
  <a href="./README.it.md">Italiano</a> |
  <a href="./README.de.md">Deutsch</a> |
  <a href="./README.pl.md">Polski</a>
</p>

---

Claudex est un proxy unifié qui permet à [Claude Code](https://docs.anthropic.com/en/docs/claude-code) de fonctionner de façon transparente avec plusieurs fournisseurs d'IA grâce à la traduction automatique de protocoles.

## Fonctionnalités

- **Proxy multi-fournisseurs** — Transfert direct DirectAnthropic + traduction Anthropic <-> OpenAI Chat Completions + traduction Anthropic <-> Responses API
- **Plus de 20 fournisseurs** — Anthropic, OpenRouter, Grok, OpenAI, DeepSeek, Kimi, GLM, Groq, Mistral, Together AI, Perplexity, Cerebras, Azure OpenAI, Google Vertex AI, Ollama, LM Studio, et plus encore
- **Traduction en streaming** — Traduction complète du flux SSE avec prise en charge des appels d'outils
- **Disjoncteur + basculement** — Repli automatique vers des fournisseurs de secours avec seuils configurables
- **Routage intelligent** — Routage automatique basé sur l'intention via un classificateur local
- **Moteur de contexte** — Compression des conversations, partage inter-profils, RAG local avec embeddings
- **Abonnements OAuth** — ChatGPT/Codex, Claude Max, GitHub Copilot, GitLab Duo, Google Gemini, Qwen, Kimi
- **Ensembles de configuration** — Installez et gérez des ensembles de configuration Claude Code réutilisables depuis des dépôts git
- **Tableau de bord TUI** — Santé des profils en temps réel, métriques, journaux et lancement rapide
- **Mise à jour automatique** — `claudex-config update` télécharge la dernière version depuis GitHub

## Installation

```bash
# En une seule commande (Linux / macOS)
curl -fsSL https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash

# Depuis les sources
cargo install --git https://github.com/pilc80/claudex

# Ou télécharger depuis GitHub Releases
# https://github.com/pilc80/claudex/releases
```

### Configuration requise

- macOS (Intel / Apple Silicon) ou Linux (x86_64 / ARM64)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) installé
- Windows : téléchargez le binaire précompilé depuis les [Releases](https://github.com/pilc80/claudex/releases)

## Démarrage rapide

```bash
# 1. Initialiser la configuration
claudex-config config init

# 2. Ajouter un profil de fournisseur de façon interactive
claudex-config profile add

# 3. Tester la connectivité
claudex-config profile test all

# 4. Lancer Claude Code avec un fournisseur spécifique
CLAUDEX_PROFILE=grok claudex

# 5. Ou utiliser le routage intelligent pour sélectionner automatiquement le meilleur fournisseur
CLAUDEX_PROFILE=auto claudex
```

## Fonctionnement

```
CLAUDEX_PROFILE=openrouter-claude claudex
    │
    ├── Démarrer le proxy (si inactif) → 127.0.0.1:13456
    │
    └── exec claude avec les variables d'environnement :
        ANTHROPIC_BASE_URL=http://127.0.0.1:13456/proxy/openrouter-claude
        ANTHROPIC_AUTH_TOKEN=claudex-passthrough
        ANTHROPIC_MODEL=anthropic/claude-sonnet-4
        ANTHROPIC_DEFAULT_HAIKU_MODEL=...
        ANTHROPIC_DEFAULT_SONNET_MODEL=...
        ANTHROPIC_DEFAULT_OPUS_MODEL=...
```

Le proxy intercepte les requêtes et gère la traduction de protocoles :

- **DirectAnthropic** (Anthropic, MiniMax, Vertex AI) → transfert avec les en-têtes corrects
- **OpenAICompatible** (Grok, OpenAI, DeepSeek, etc.) → Anthropic → OpenAI Chat Completions → traduction de la réponse
- **OpenAIResponses** (abonnements ChatGPT/Codex) → Anthropic → Responses API → traduction de la réponse

## Compatibilité des fournisseurs

| Fournisseur | Type | Traduction | Auth | Exemple de modèle |
|-------------|------|------------|------|-------------------|
| Anthropic | DirectAnthropic | Aucune | API Key | `claude-sonnet-4-20250514` |
| MiniMax | DirectAnthropic | Aucune | API Key | `claude-sonnet-4-20250514` |
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
| Azure OpenAI | OpenAICompatible | Anthropic <-> OpenAI | en-tête api-key | `gpt-4o` |
| Google Vertex AI | DirectAnthropic | Aucune | Bearer (gcloud) | `claude-sonnet-4@...` |
| Ollama | OpenAICompatible | Anthropic <-> OpenAI | Aucune | `qwen2.5:72b` |
| LM Studio | OpenAICompatible | Anthropic <-> OpenAI | Aucune | modèle local |
| Abonnement ChatGPT/Codex | OpenAIResponses | Anthropic <-> Responses | OAuth (PKCE/Device) | `gpt-5.5` |
| Abonnement Claude Max | DirectAnthropic | Aucune | OAuth (fichier) | `claude-sonnet-4` |
| GitHub Copilot | OpenAICompatible | Anthropic <-> OpenAI | OAuth (Device+Bearer) | `gpt-4o` |
| GitLab Duo | OpenAICompatible | Anthropic <-> OpenAI | GITLAB_TOKEN | `claude-sonnet-4` |

## Configuration

Claudex recherche les fichiers de configuration dans cet ordre :

1. Variable d'environnement `$CLAUDEX_CONFIG`
2. `./claudex.toml` ou `./claudex.yaml` (répertoire courant)
3. `./.claudex/config.toml`
4. Répertoires parents (jusqu'à 10 niveaux)
5. `~/.config/claudex/config.toml` (global, recommandé)

Prend en charge les formats TOML et YAML. Consultez [`config.example.toml`](./config.example.toml) pour la référence complète.

## Référence CLI

| Commande | Description |
|----------|-------------|
| `CLAUDEX_PROFILE=<profile> claudex` | Lancer Claude Code avec un fournisseur spécifique |
| `CLAUDEX_PROFILE=auto claudex` | Routage intelligent — sélectionner automatiquement le meilleur fournisseur |
| `CLAUDEX_PROFILE=<profile> CLAUDEX_MODEL=<model> claudex` | Remplacer le modèle pour une session |
| `claudex-config profile list` | Lister tous les profils configurés |
| `claudex-config profile add` | Assistant de configuration de profil interactif |
| `claudex-config profile show <name>` | Afficher les détails d'un profil |
| `claudex-config profile remove <name>` | Supprimer un profil |
| `claudex-config profile test <name\|all>` | Tester la connectivité du fournisseur |
| `claudex-config proxy start [-p port] [-d]` | Démarrer le proxy (optionnellement en tant que démon) |
| `claudex-config proxy stop` | Arrêter le démon proxy |
| `claudex-config proxy status` | Afficher l'état du proxy |
| `claudex-config dashboard` | Lancer le tableau de bord TUI |
| `claudex-config config show [--raw] [--json]` | Afficher la configuration chargée |
| `claudex-config config init [--yaml]` | Créer une configuration dans le répertoire courant |
| `claudex-config config edit [--global]` | Ouvrir la configuration dans $EDITOR |
| `claudex-config config validate [--connectivity]` | Valider la configuration |
| `claudex-config config get <key>` | Obtenir une valeur de configuration |
| `claudex-config config set <key> <value>` | Définir une valeur de configuration |
| `claudex-config config export --format <fmt>` | Exporter la configuration (json/toml/yaml) |
| `claudex-config update [--check]` | Mise à jour automatique depuis GitHub Releases |
| `claudex-config auth login <provider>` | Connexion OAuth |
| `claudex-config auth login github --enterprise-url <domain>` | GitHub Enterprise Copilot |
| `claudex-config auth status` | Afficher l'état des tokens OAuth |
| `claudex-config auth logout <profile>` | Supprimer un token OAuth |
| `claudex-config auth refresh <profile>` | Forcer le renouvellement du token OAuth |
| `claudex-config sets add <source> [--global]` | Installer un ensemble de configuration |
| `claudex-config sets remove <name>` | Supprimer un ensemble de configuration |
| `claudex-config sets list [--global]` | Lister les ensembles installés |
| `claudex-config sets update [name]` | Mettre à jour les ensembles vers la dernière version |

## Abonnements OAuth

Utilisez vos abonnements existants à la place des clés API :

```bash
# Abonnement ChatGPT (détecte automatiquement les identifiants Codex CLI existants)
claudex-config auth login chatgpt --profile codex-sub

# ChatGPT avec connexion navigateur forcée
claudex-config auth login chatgpt --profile codex-sub --force

# ChatGPT sans interface graphique (SSH/sans navigateur)
claudex-config auth login chatgpt --profile codex-sub --force --headless

# GitHub Copilot
claudex-config auth login github --profile copilot

# GitHub Copilot Enterprise
claudex-config auth login github --profile copilot-ent --enterprise-url company.ghe.com

# GitLab Duo (lit la variable d'environnement GITLAB_TOKEN)
claudex-config auth login gitlab --profile gitlab-duo

# Vérifier le statut
claudex-config auth status

# Lancer avec l'abonnement
CLAUDEX_PROFILE=codex-sub claudex
```

Pris en charge : `claude`, `chatgpt`/`openai`, `google`, `qwen`, `kimi`, `github`/`copilot`, `gitlab`

## Correspondance des slots de modèles

Associez le sélecteur `/model` de Claude Code (haiku/sonnet/opus) aux modèles de n'importe quel fournisseur :

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

## Architecture

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
│   ├── mod.rs          # Découverte + analyse de configuration (figment)
│   ├── cmd.rs          # Sous-commandes config get/set/export/validate
│   └── profile.rs      # CRUD des profils + test de connectivité
├── process/
│   ├── mod.rs
│   ├── launch.rs       # Lanceur de processus Claude
│   └── daemon.rs       # Fichier PID + gestion des processus
├── oauth/
│   ├── mod.rs          # AuthType, OAuthProvider, OAuthToken
│   ├── source.rs       # Couche 1 : sources d'identifiants (env/fichier/keyring)
│   ├── exchange.rs     # Couche 2 : échange de tokens (PKCE/device code/refresh)
│   ├── manager.rs      # Couche 3 : cache + déduplication concurrente + retry 401
│   ├── handler.rs      # Trait OAuthProviderHandler
│   ├── providers.rs    # Logique CLI de connexion/refresh/statut
│   ├── server.rs       # Serveur de rappel OAuth + interrogation device code
│   └── token.rs        # Réexportations
├── proxy/
│   ├── mod.rs          # Serveur Axum + ProxyState
│   ├── handler.rs      # Routage des requêtes + disjoncteur + retry 401
│   ├── adapter/        # Adaptateurs spécifiques aux fournisseurs
│   │   ├── mod.rs      # Trait ProviderAdapter + factory
│   │   ├── direct.rs   # DirectAnthropic (transfert direct)
│   │   ├── chat_completions.rs  # OpenAI Chat Completions
│   │   └── responses.rs         # OpenAI Responses API
│   ├── translate/      # Traduction de protocoles
│   │   ├── chat_completions.rs
│   │   ├── chat_completions_stream.rs
│   │   ├── responses.rs
│   │   └── responses_stream.rs
│   ├── context_engine.rs
│   ├── fallback.rs     # Disjoncteur
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
├── sets/               # Gestion des ensembles de configuration
│   ├── mod.rs
│   ├── schema.rs
│   ├── source.rs
│   ├── install.rs
│   ├── lock.rs
│   ├── conflict.rs
│   └── mcp.rs
├── terminal/           # Détection du terminal + hyperliens
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

## Licence

[MIT](./LICENSE)
