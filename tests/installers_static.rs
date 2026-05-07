use std::fs;

fn install_ps1() -> String {
    fs::read_to_string("install.ps1").expect("install.ps1 should exist")
}

#[test]
fn windows_installer_uses_safe_web_wrappers() {
    let script = install_ps1();

    assert!(script.contains("function Invoke-InstallerRestMethod"));
    assert!(script.contains("function Invoke-InstallerWebRequest"));
    assert!(script.contains("SecurityProtocolType]::Tls12"));
    assert!(script.contains("UseBasicParsing"));
    assert!(script.contains("UserAgent = \"claudex-installer\""));
}

#[test]
fn unix_installer_downloads_with_progress_timeout_and_retries() {
    let script = fs::read_to_string("install.sh").expect("install.sh should exist");

    assert!(script.contains("--connect-timeout"));
    assert!(script.contains("--retry"));
    assert!(!script.contains("--progress-bar"));
    assert!(script.contains("Downloading release manifest"));
    assert!(script.contains("Downloading release archive"));
    assert!(script.contains("Downloaded release manifest"));
    assert!(script.contains("Downloaded release archive"));
    assert!(script.contains("Downloaded checksum"));
    assert!(!script.contains("say \"Downloading: $url\""));
}

#[test]
fn unix_installer_replaces_existing_path_install_and_verifies_latest() {
    let script = fs::read_to_string("install.sh").expect("install.sh should exist");

    assert!(script.contains("EXPLICIT_INSTALL_DIR"));
    assert!(script.contains("select_install_dir"));
    assert!(script.contains("resolve_command_path"));
    assert!(script.contains("EXPECTED_VERSION"));
    assert!(script.contains("verify_installed_latest"));
    assert!(script.contains("PATH resolves claudex-config to"));
}

#[test]
fn unix_installer_avoids_global_dest_variable_collision() {
    let script = fs::read_to_string("install.sh").expect("install.sh should exist");

    assert!(script.contains("backup_path=\"$1\""));
    assert!(script.contains("binary_dest=\"$INSTALL_DIR/claudex\""));
    assert!(script.contains("mv \"$src\" \"$binary_dest\""));
    assert!(!script.contains("dest=\"$1\""));
}

#[test]
fn unix_installer_explains_running_proxy_action() {
    let script = fs::read_to_string("install.sh").expect("install.sh should exist");

    assert!(script.contains("Installation complete."));
    assert!(script.contains("Action needed:"));
    assert!(script.contains("The old proxy is still running"));
    let action_block = script
        .split("Action needed:")
        .nth(1)
        .expect("installer should explain post-install proxy action")
        .split("maybe_setup_chatgpt")
        .next()
        .unwrap_or("");
    assert!(action_block.contains("claudex-config proxy stop"));
    assert!(action_block.contains("  claudex"));
    assert!(!action_block.contains("CLAUDEX_PROFILE=$PROFILE_NAME claudex"));
}

#[test]
fn installers_use_claudex_config_for_management() {
    let unix_installer = fs::read_to_string("install.sh").expect("install.sh should exist");
    let windows_installer = install_ps1();
    let release_workflow =
        fs::read_to_string(".github/workflows/release.yml").expect("release workflow should exist");

    assert!(unix_installer.contains("claudex-config"));
    assert!(unix_installer.contains("\"$INSTALLED_CONFIG_BIN\" auth login chatgpt"));
    assert!(unix_installer.contains("\"$INSTALLED_CONFIG_BIN\" proxy status"));
    assert!(windows_installer.contains("claudex-config.exe"));
    assert!(windows_installer.contains("@(\"auth\", \"login\", \"chatgpt\""));
    assert!(windows_installer.contains("@(\"proxy\", \"status\")"));
    assert!(release_workflow.contains("claudex-config"));
}

#[test]
fn claudex_binary_dispatches_by_argv0_for_config_links() {
    let launcher = fs::read_to_string("src/bin/claudex.rs").expect("launcher bin should exist");

    assert!(launcher.contains("run_from_argv0"));
}

#[test]
fn windows_installer_checks_native_exit_codes() {
    let script = install_ps1();

    assert!(script.contains("function Invoke-Native"));
    assert!(script.contains("$global:LASTEXITCODE"));
    assert!(script.contains("throw \"$Name failed with exit code $global:LASTEXITCODE\""));
}

#[test]
fn windows_installer_falls_back_to_source_then_deploys_same_path() {
    let script = install_ps1();

    assert!(script.contains("function Install-FromRelease"));
    assert!(script.contains("function Install-FromSource"));
    assert!(script.contains("function Deploy-Binary"));
    assert!(script.contains("$source = Install-FromRelease"));
    assert!(script.contains("$source = Install-FromSource"));
    assert!(script.contains("Deploy-Binary $source"));
}

#[test]
fn windows_installer_stops_running_proxy_before_overwrite() {
    let script = install_ps1();
    let stop_pos = script
        .find("Maybe-StopRunningProxy $dest")
        .expect("installer should check running proxy before deploy");
    let copy_pos = script
        .find("Copy-Item -LiteralPath $source -Destination $dest -Force")
        .expect("installer should copy extracted binary into place");

    assert!(
        stop_pos < copy_pos,
        "running proxy check must happen before overwriting claudex.exe"
    );
}

#[test]
fn windows_installer_reports_unsupported_arm64() {
    let script = install_ps1();

    assert!(script.contains("PROCESSOR_ARCHITEW6432"));
    assert!(script.contains("PROCESSOR_ARCHITECTURE"));
    assert!(script.contains("Windows ARM64 is not supported"));
}

#[test]
fn windows_installer_validates_repo_and_avoids_noninteractive_prompts() {
    let script = install_ps1();

    assert!(script.contains("function Assert-ValidRepo"));
    assert!(script.contains("^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$"));
    assert!(script.contains("function Test-Interactive"));
    assert!(script.contains("function Read-InstallerInput"));
    assert!(script.contains("[Console]::IsInputRedirected"));
    assert!(script.contains("if (-not (Test-Interactive)) { return $Default }"));
    assert!(script.contains("Read-InstallerInput \"Profile name\" $Profile"));
}

#[test]
fn windows_installer_handles_same_file_deploy_and_temp_cleanup() {
    let script = install_ps1();

    assert!(script.contains("function Resolve-FullPath"));
    assert!(script.contains("Skipping copy because source and destination are the same file"));
    assert!(script.contains("function Test-InstallerTempSource"));
    assert!(script.contains("finally"));
    assert!(
        script.contains("Remove-Item -LiteralPath $source -Force -ErrorAction SilentlyContinue")
    );
}

#[test]
fn windows_installer_stages_new_binary_before_replacing_existing_one() {
    let script = install_ps1();
    assert!(script.contains(".claudex.new."));
    assert!(script.contains("staged claudex --version"));
    assert!(script.contains("Move-Item -LiteralPath $stagingDest -Destination $dest -Force"));
    assert!(script.contains(".claudex-config.new."));
    assert!(script.contains("staged claudex-config --version"));
    assert!(script
        .contains("Move-Item -LiteralPath $stagingConfigDest -Destination $configDest -Force"));
}

#[test]
fn windows_installer_stops_cargo_bin_proxy_before_source_install() {
    let script = install_ps1();
    let stop_pos = script
        .find("Maybe-StopRunningProxy $source")
        .expect("source install should check cargo-bin claudex before cargo install");
    let cargo_pos = script
        .find("Invoke-Native \"cargo install\"")
        .expect("source install should run cargo install");

    assert!(
        stop_pos < cargo_pos,
        "cargo-bin proxy check must happen before cargo install can overwrite claudex.exe"
    );
}
