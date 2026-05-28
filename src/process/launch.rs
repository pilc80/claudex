use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::config::{ClaudexConfig, HyperlinksConfig, ProfileConfig};
use crate::oauth::{AuthType, OAuthProvider};
use crate::terminal;

const CLAUDEX_WEBSEARCH_POLICY_PROMPT: &str = "Web research: DO NOT use WebSearch through the proxy. For known URLs use WebFetch. For search use GitHub/gh, MCP search, or Bash/curl to a local search provider. Do not delegate web research/search to subagents.";

pub fn launch_claude(
    config: &ClaudexConfig,
    profile: &ProfileConfig,
    model_override: Option<&str>,
    extra_args: &[String],
    hyperlinks_override: bool,
) -> Result<()> {
    let proxy_base = format!(
        "http://{}:{}/proxy/{}",
        config.proxy_host, config.proxy_port, profile.name
    );

    let model = model_override
        .map(|m| config.resolve_model(m))
        .unwrap_or_else(|| config.resolve_model(&profile.default_model));
    let is_openai_responses_oauth = is_openai_responses_oauth_profile(profile);
    let visible_model = claude_visible_model(&model, is_openai_responses_oauth);

    // 非交互模式检测：含 -p / --print，或首个 arg 不是 flag（裸 prompt）
    let is_noninteractive = extra_args.iter().any(|arg| arg == "-p" || arg == "--print")
        || extra_args.first().is_some_and(|arg| !arg.starts_with('-'));

    let is_claude_subscription = profile.auth_type == AuthType::OAuth
        && profile.oauth_provider == Some(OAuthProvider::Claude);
    let guard_support = claude_guard_support(&config.claude_binary);
    let command_context = ClaudeCommandContext {
        config,
        profile,
        proxy_base: &proxy_base,
        visible_model: &visible_model,
        is_claude_subscription,
        is_openai_responses_oauth,
        extra_args,
    };
    let mut cmd = build_claude_command(&command_context, Some(guard_support));

    tracing::info!(
        profile = %profile.name,
        model = %model,
        proxy = %proxy_base,
        noninteractive = %is_noninteractive,
        "launching claude"
    );

    // PTY mode (Unix only): 非交互模式跳过 PTY
    #[cfg(unix)]
    let use_pty = !is_noninteractive && should_use_pty(&config.hyperlinks, hyperlinks_override);
    #[cfg(not(unix))]
    let use_pty = false;

    let mut resume_session_id: Option<String> = None;

    if use_pty {
        #[cfg(unix)]
        {
            tracing::info!("hyperlinks enabled, using PTY proxy mode");
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
            resume_session_id = terminal::pty::spawn_with_pty(cmd, cwd)?;
        }
    } else {
        let stderr_output = if guard_support.has_any() && is_noninteractive {
            cmd.stderr(Stdio::piped());
            Some(run_claude_child(cmd, true)?)
        } else {
            run_claude_child(cmd, false)?;
            None
        };

        if let Some(stderr) = stderr_output {
            if is_unknown_guard_arg_error(&stderr) {
                eprintln!(
                    "Claudex warning: Claude Code rejected WebSearch guard args; retrying without them."
                );
                let retry = build_claude_command(&command_context, None);
                run_claude_child(retry, false)?;
            } else {
                eprint!("{stderr}");
                bail!("claude exited with an error");
            }
        }
    }

    // 追加 claudex resume 命令提示
    if let Some(session_id) = resume_session_id {
        print_claudex_resume_hint(&profile.name, &session_id, extra_args);
    }

    Ok(())
}

/// 在 Claude Code 退出后追加 claudex resume 命令提示
fn print_claudex_resume_hint(profile_name: &str, session_id: &str, extra_args: &[String]) {
    let hint = build_resume_hint(profile_name, session_id, extra_args);
    eprintln!("\nResume this session with claudex:\n  {hint}");
}

struct ClaudeCommandContext<'a> {
    config: &'a ClaudexConfig,
    profile: &'a ProfileConfig,
    proxy_base: &'a str,
    visible_model: &'a str,
    is_claude_subscription: bool,
    is_openai_responses_oauth: bool,
    extra_args: &'a [String],
}

fn build_claude_command(
    ctx: &ClaudeCommandContext<'_>,
    guard_support: Option<ClaudeGuardSupport>,
) -> Command {
    let mut cmd = Command::new(&ctx.config.claude_binary);

    if ctx.is_claude_subscription {
        if ctx.visible_model != ctx.profile.default_model {
            cmd.env("ANTHROPIC_MODEL", ctx.visible_model);
        }
    } else {
        cmd.env("ANTHROPIC_BASE_URL", ctx.proxy_base)
            .env("ANTHROPIC_AUTH_TOKEN", "claudex-passthrough")
            .env("ANTHROPIC_MODEL", ctx.visible_model);
    }

    if !ctx.profile.custom_headers.is_empty() {
        let headers: Vec<String> = ctx
            .profile
            .custom_headers
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect();
        cmd.env("ANTHROPIC_CUSTOM_HEADERS", headers.join(","));
    }

    if let Some(ref h) = ctx.profile.models.haiku {
        cmd.env("ANTHROPIC_DEFAULT_HAIKU_MODEL", h);
    }
    if let Some(ref s) = ctx.profile.models.sonnet {
        cmd.env("ANTHROPIC_DEFAULT_SONNET_MODEL", s);
    }
    if let Some(ref o) = ctx.profile.models.opus {
        cmd.env("ANTHROPIC_DEFAULT_OPUS_MODEL", o);
    }

    if ctx.is_openai_responses_oauth {
        if let Some(window) = openai_model_auto_compact_window(ctx.visible_model) {
            cmd.env("CLAUDE_CODE_AUTO_COMPACT_WINDOW", window.to_string());
        }
    }

    for (k, v) in &ctx.profile.extra_env {
        cmd.env(k, v);
    }

    if !ctx.extra_args.iter().any(|a| a == "--chrome") {
        cmd.arg("--no-chrome");
    }

    let claude_args = match guard_support {
        Some(support) => claudex_websearch_guard_args(ctx.extra_args, support),
        None => ctx.extra_args.to_vec(),
    };
    cmd.args(&claude_args);
    cmd
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaudeGuardSupport {
    allowed_tools: Option<&'static str>,
    disallowed_tools: Option<&'static str>,
    append_system_prompt: bool,
}

impl ClaudeGuardSupport {
    fn has_any(self) -> bool {
        self.allowed_tools.is_some() || self.disallowed_tools.is_some() || self.append_system_prompt
    }
}

fn claude_guard_support(claude_binary: &str) -> ClaudeGuardSupport {
    let help = Command::new(claude_binary).arg("--help").output();
    match help {
        Ok(output) => parse_claude_guard_support(&String::from_utf8_lossy(&output.stdout)),
        Err(err) => {
            eprintln!("Claudex warning: could not inspect Claude Code flags: {err}");
            ClaudeGuardSupport {
                allowed_tools: None,
                disallowed_tools: None,
                append_system_prompt: false,
            }
        }
    }
}

fn parse_claude_guard_support(help: &str) -> ClaudeGuardSupport {
    ClaudeGuardSupport {
        allowed_tools: if help.contains("--allowedTools") {
            Some("--allowedTools")
        } else if help.contains("--allowed-tools") {
            Some("--allowed-tools")
        } else {
            None
        },
        disallowed_tools: if help.contains("--disallowedTools") {
            Some("--disallowedTools")
        } else if help.contains("--disallowed-tools") {
            Some("--disallowed-tools")
        } else {
            None
        },
        append_system_prompt: help.contains("--append-system-prompt"),
    }
}

fn run_claude_child(mut cmd: Command, capture_stderr: bool) -> Result<String> {
    if capture_stderr {
        let output = cmd.output().context("failed to execute claude binary")?;
        if output.status.success() {
            return Ok(String::new());
        }
        return Ok(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let mut child = cmd.spawn().context("failed to execute claude binary")?;

    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }

    let status = child.wait().context("failed to wait for claude")?;

    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }

    if status.success() {
        return Ok(String::new());
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status.signal().is_some() {
            std::process::exit(128 + status.signal().unwrap());
        }
    }

    bail!("claude exited with status: {status}")
}

fn is_unknown_guard_arg_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    (lower.contains("unknown") || lower.contains("unexpected") || lower.contains("invalid"))
        && [
            "allowedtools",
            "allowed-tools",
            "disallowedtools",
            "disallowed-tools",
            "append-system-prompt",
        ]
        .iter()
        .any(|flag| lower.contains(flag))
}

fn claudex_websearch_guard_args(
    extra_args: &[String],
    guard_support: ClaudeGuardSupport,
) -> Vec<String> {
    let mut args = Vec::with_capacity(extra_args.len() + 6);

    if let Some(flag) = guard_support.disallowed_tools {
        if !has_flag_value(extra_args, flag, "WebSearch") {
            args.push(flag.to_string());
            args.push("WebSearch".to_string());
        }
    }

    if let Some(flag) = guard_support.allowed_tools {
        if !has_flag_value(extra_args, flag, "WebFetch") {
            args.push(flag.to_string());
            args.push("WebFetch".to_string());
        }
    }

    if guard_support.append_system_prompt {
        args.push("--append-system-prompt".to_string());
        args.push(CLAUDEX_WEBSEARCH_POLICY_PROMPT.to_string());
    }

    if !guard_support.has_any() {
        eprintln!("Claudex warning: Claude Code does not advertise WebSearch guard flags; launching without injected guardrails.");
    }

    args.extend(extra_args.iter().cloned());
    args
}

fn has_flag_value(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1].split(',').any(|part| part.trim() == value))
}

/// 构造 claudex resume 命令字符串（纯函数，便于测试）
fn build_resume_hint(profile_name: &str, session_id: &str, extra_args: &[String]) -> String {
    // 过滤掉原始 extra_args 中的 --resume 及其值参数
    let mut args_clean: Vec<&str> = Vec::new();
    let mut skip_next = false;
    for arg in extra_args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--resume" {
            skip_next = true;
            continue;
        }
        args_clean.push(arg);
    }

    let args_str = if args_clean.is_empty() {
        String::new()
    } else {
        format!(" {}", args_clean.join(" "))
    };

    format!("CLAUDEX_PROFILE={profile_name} claudex --resume {session_id}{args_str}")
}

/// Decide whether to use PTY mode based on config + CLI flag.
fn is_openai_responses_oauth_profile(profile: &ProfileConfig) -> bool {
    profile.provider_type == crate::config::ProviderType::OpenAIResponses
        && profile.auth_type == AuthType::OAuth
        && profile
            .oauth_provider
            .as_ref()
            .is_some_and(|provider| provider.normalize() == OAuthProvider::Chatgpt)
}

fn claude_visible_model(model: &str, enable_openai_context_window: bool) -> String {
    if !enable_openai_context_window
        || has_context_window_suffix(model)
        || !is_large_context_gpt_model(strip_context_window_suffix(model))
    {
        model.to_string()
    } else {
        format!("{model}[1m]")
    }
}

fn openai_model_auto_compact_window(model: &str) -> Option<u64> {
    let model = strip_context_window_suffix(model);
    match model {
        model if is_large_context_gpt_model(model) => None,
        model if is_openai_gpt_model(model) => Some(272_000),
        _ => None,
    }
}

fn strip_context_window_suffix(model: &str) -> &str {
    model
        .strip_suffix("[1m]")
        .or_else(|| model.strip_suffix("[1M]"))
        .unwrap_or(model)
}

fn has_context_window_suffix(model: &str) -> bool {
    strip_context_window_suffix(model) != model
}

fn is_large_context_gpt_model(model: &str) -> bool {
    if model == "gpt-5.5-pro" {
        return true;
    }

    let Some(version) = model.strip_prefix("gpt-") else {
        return false;
    };

    let mut parts = version.split(['.', '-']);
    let Some(Ok(major)) = parts.next().map(str::parse::<u64>) else {
        return false;
    };
    let minor = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .unwrap_or(0);

    major > 5 || major == 5 && minor > 5
}

fn is_openai_gpt_model(model: &str) -> bool {
    model.starts_with("gpt-")
}

#[cfg(unix)]
fn should_use_pty(config_hyperlinks: &HyperlinksConfig, cli_override: bool) -> bool {
    if cli_override {
        return true;
    }

    match config_hyperlinks {
        HyperlinksConfig::Enabled => true,
        HyperlinksConfig::Disabled => false,
        HyperlinksConfig::Auto => terminal::detect::terminal_supports_hyperlinks(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claudex_websearch_guard_adds_default_args_before_user_args() {
        let support = ClaudeGuardSupport {
            allowed_tools: Some("--allowedTools"),
            disallowed_tools: Some("--disallowedTools"),
            append_system_prompt: true,
        };
        let args = claudex_websearch_guard_args(&["--verbose".to_string()], support);
        assert_eq!(
            args,
            vec![
                "--disallowedTools",
                "WebSearch",
                "--allowedTools",
                "WebFetch",
                "--append-system-prompt",
                CLAUDEX_WEBSEARCH_POLICY_PROMPT,
                "--verbose"
            ]
        );
    }

    #[test]
    fn claudex_websearch_guard_does_not_duplicate_tool_flags() {
        let support = ClaudeGuardSupport {
            allowed_tools: Some("--allowedTools"),
            disallowed_tools: Some("--disallowedTools"),
            append_system_prompt: true,
        };
        let args = claudex_websearch_guard_args(
            &[
                "--disallowedTools".to_string(),
                "Bash,WebSearch".to_string(),
                "--allowedTools".to_string(),
                "Read, WebFetch".to_string(),
            ],
            support,
        );

        assert_eq!(
            args,
            vec![
                "--append-system-prompt",
                CLAUDEX_WEBSEARCH_POLICY_PROMPT,
                "--disallowedTools",
                "Bash,WebSearch",
                "--allowedTools",
                "Read, WebFetch"
            ]
        );
    }

    #[test]
    fn claudex_websearch_guard_uses_kebab_case_flags_when_advertised() {
        let support = ClaudeGuardSupport {
            allowed_tools: Some("--allowed-tools"),
            disallowed_tools: Some("--disallowed-tools"),
            append_system_prompt: true,
        };
        let args = claudex_websearch_guard_args(&[], support);
        assert_eq!(args[0], "--disallowed-tools");
        assert_eq!(args[2], "--allowed-tools");
    }

    #[test]
    fn parse_claude_guard_support_prefers_camel_case_flags() {
        let support = parse_claude_guard_support(
            "--allowedTools, --allowed-tools <tools>\n--disallowedTools <tools>\n--append-system-prompt <prompt>",
        );
        assert_eq!(support.allowed_tools, Some("--allowedTools"));
        assert_eq!(support.disallowed_tools, Some("--disallowedTools"));
        assert!(support.append_system_prompt);
    }

    #[test]
    fn unknown_guard_arg_error_requires_guard_flag_reference() {
        assert!(is_unknown_guard_arg_error(
            "error: unknown option '--append-system-prompt'"
        ));
        assert!(!is_unknown_guard_arg_error(
            "error: unknown option '--model'"
        ));
    }

    #[test]
    fn test_build_resume_hint_no_extra_args() {
        let hint = build_resume_hint("codex-sub", "abc-123", &[]);
        assert_eq!(hint, "CLAUDEX_PROFILE=codex-sub claudex --resume abc-123");
    }

    #[test]
    fn test_build_resume_hint_with_extra_args() {
        let args = vec![
            "--dangerously-skip-permissions".to_string(),
            "--verbose".to_string(),
        ];
        let hint = build_resume_hint("codex-sub", "abc-123", &args);
        assert_eq!(
            hint,
            "CLAUDEX_PROFILE=codex-sub claudex --resume abc-123 --dangerously-skip-permissions --verbose"
        );
    }

    #[test]
    fn test_build_resume_hint_filters_existing_resume() {
        let args = vec![
            "--resume".to_string(),
            "old-session-id".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        let hint = build_resume_hint("codex-sub", "new-session-id", &args);
        assert_eq!(
            hint,
            "CLAUDEX_PROFILE=codex-sub claudex --resume new-session-id --dangerously-skip-permissions"
        );
    }

    #[test]
    fn test_build_resume_hint_resume_at_end() {
        let args = vec![
            "--verbose".to_string(),
            "--resume".to_string(),
            "old-id".to_string(),
        ];
        let hint = build_resume_hint("my-profile", "new-id", &args);
        assert_eq!(
            hint,
            "CLAUDEX_PROFILE=my-profile claudex --resume new-id --verbose"
        );
    }

    #[test]
    fn large_context_gpt_detection_matches_boundary() {
        assert!(["gpt-5.5-pro", "gpt-5.6"]
            .into_iter()
            .all(is_large_context_gpt_model));
        assert!(["gpt-5.5", "gpt-5.5-mini", "gpt-5.4-pro"]
            .into_iter()
            .all(|model| !is_large_context_gpt_model(model)));
    }

    #[test]
    fn claude_visible_model_adds_suffix_only_for_large_context_models() {
        assert_eq!(claude_visible_model("gpt-5.5", true), "gpt-5.5");
        assert_eq!(claude_visible_model("gpt-5.5-mini", true), "gpt-5.5-mini");
        assert_eq!(claude_visible_model("gpt-4o", true), "gpt-4o");
        assert_eq!(claude_visible_model("gpt-5.5-pro", true), "gpt-5.5-pro[1m]");
        assert_eq!(claude_visible_model("gpt-5.6", true), "gpt-5.6[1m]");
        assert_eq!(claude_visible_model("gpt-5.6[1m]", true), "gpt-5.6[1m]");
        assert_eq!(claude_visible_model("gpt-5.6[1M]", true), "gpt-5.6[1M]");
        assert_eq!(claude_visible_model("gpt-5.6", false), "gpt-5.6");
        assert_eq!(
            claude_visible_model("claude-sonnet-4-6", true),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn openai_model_auto_compact_window_uses_legacy_window_for_non_large_gpt_models() {
        assert_eq!(openai_model_auto_compact_window("gpt-5.5"), Some(272_000));
        assert_eq!(
            openai_model_auto_compact_window("gpt-5.5-mini"),
            Some(272_000)
        );
        assert_eq!(openai_model_auto_compact_window("gpt-4o"), Some(272_000));
        assert_eq!(openai_model_auto_compact_window("gpt-5.5-pro"), None);
        assert_eq!(openai_model_auto_compact_window("gpt-5.6"), None);
        assert_eq!(openai_model_auto_compact_window("claude-sonnet-4-6"), None);
    }

    #[test]
    fn openai_responses_oauth_profile_enables_context_window_override() {
        let mut profile = ProfileConfig {
            provider_type: crate::config::ProviderType::OpenAIResponses,
            auth_type: AuthType::OAuth,
            oauth_provider: Some(OAuthProvider::Chatgpt),
            ..Default::default()
        };
        assert!(is_openai_responses_oauth_profile(&profile));

        profile.oauth_provider = Some(OAuthProvider::Claude);
        assert!(!is_openai_responses_oauth_profile(&profile));
    }
}
