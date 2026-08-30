//! 仅限 root 执行的自安装与 systemd 服务配置。

use crate::config::{Config, set_mode};
use crate::constants::{
    ADMIN_GROUP, BINARY_PATH, CONFIG_DIR, UNIT_PATH, auth_token_path, config_path,
};
use anyhow::Context;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// 用户提供的初始化覆盖项。
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// 可选基础域名。
    pub domain: Option<String>,
    /// 可选项目根目录。
    pub stacks_root: Option<PathBuf>,
    /// 可选的完整 bind mount 根目录列表。
    pub data_roots: Option<Vec<PathBuf>>,
    /// 可选 gRPC 监听地址。
    pub listen: Option<String>,
    /// 可选 Docker socket。
    pub docker_socket: Option<PathBuf>,
    /// 是否允许替换现有安装。
    pub force: bool,
}

/// 安装当前二进制、配置文件和 systemd unit。
///
/// 使用 `force` 时，未指定的配置值沿用现有配置。
///
/// # 错误
///
/// 未以 root 运行或任一安装步骤失败时返回错误。
pub fn install(options: InstallOptions) -> anyhow::Result<()> {
    ensure_root()?;
    let path = config_path();
    if path.exists() && !options.force {
        anyhow::bail!("nsetup 已初始化；确认覆盖请使用 --force");
    }
    let mut config = Config::load_or_default()?;
    if let Some(domain) = options.domain {
        config.domain = domain;
    }
    if let Some(stacks_root) = options.stacks_root {
        config.stacks_root = stacks_root;
    }
    if let Some(data_roots) = options.data_roots {
        config.data_roots = data_roots;
    }
    if let Some(listen) = options.listen {
        config.listen = listen;
    }
    if let Some(docker_socket) = options.docker_socket {
        config.docker_socket = docker_socket;
    }
    config.validate()?;
    ensure_group()?;
    fs::create_dir_all(CONFIG_DIR)?;
    fs::create_dir_all(&config.stacks_root)?;
    for root in &config.data_roots {
        fs::create_dir_all(root)
            .with_context(|| format!("无法创建数据目录: {}", root.display()))?;
        set_mode(root, 0o750)?;
    }
    set_mode(&config.stacks_root, 0o750)?;
    install_binary()?;
    write_secure_file(&path, toml::to_string_pretty(&config)?.as_bytes(), 0o640)?;
    chown_group(&path)?;
    if !config.listen.starts_with("unix://") {
        ensure_auth_token()?;
    }
    write_secure_file(
        Path::new(UNIT_PATH),
        systemd_unit(&config).as_bytes(),
        0o644,
    )?;
    run_checked(
        Command::new("systemctl").arg("daemon-reload"),
        "systemctl daemon-reload",
    )?;
    run_checked(
        Command::new("systemctl").args(["enable", "nsetup.service"]),
        "systemctl enable nsetup.service",
    )?;
    run_checked(
        Command::new("systemctl").args(["restart", "nsetup.service"]),
        "systemctl restart nsetup.service",
    )?;
    Ok(())
}

/// 确保 TCP 认证令牌存在并返回令牌。
///
/// # 错误
///
/// 无法安全读取或创建令牌文件时返回错误。
pub fn ensure_auth_token() -> anyhow::Result<String> {
    let path = auth_token_path();
    match fs::read_to_string(&path) {
        Ok(value) if !value.trim().is_empty() => return Ok(value.trim().to_string()),
        Ok(_) => anyhow::bail!("认证 token 文件为空: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let bytes = rand::random::<[u8; 32]>();
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    write_secure_file(&path, format!("{token}\n").as_bytes(), 0o600)?;
    Ok(token)
}

/// 确认当前进程以 root 身份运行。
///
/// # 错误
///
/// `id` 执行失败或有效用户不是 root 时返回错误。
fn ensure_root() -> anyhow::Result<()> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("无法检查当前用户")?;
    ensure_success(&output, "id -u")?;
    if String::from_utf8_lossy(&output.stdout).trim() != "0" {
        anyhow::bail!("nsetup init 必须以 root 运行");
    }
    Ok(())
}

/// 管理员用户组不存在时创建该用户组。
///
/// # 错误
///
/// 用户组查询或创建失败时返回错误。
fn ensure_group() -> anyhow::Result<()> {
    let status = Command::new("getent")
        .args(["group", ADMIN_GROUP])
        .status()
        .context("无法查询 nihility 用户组")?;
    if status.success() {
        return Ok(());
    }
    run_checked(
        Command::new("groupadd").args(["--system", ADMIN_GROUP]),
        "创建 nihility 用户组",
    )
}

/// 将当前可执行文件原子复制到固定系统路径。
///
/// # 错误
///
/// 无法复制或替换可执行文件时返回错误。
fn install_binary() -> anyhow::Result<()> {
    let source = std::env::current_exe().context("无法定位当前 nsetup 可执行文件")?;
    let destination = Path::new(BINARY_PATH);
    if source == destination {
        return Ok(());
    }
    let temporary = destination.with_file_name(format!(".nsetup.bin-{}", std::process::id()));
    fs::copy(&source, &temporary).with_context(|| {
        format!(
            "无法复制二进制 {} -> {}",
            source.display(),
            temporary.display()
        )
    })?;
    set_mode(&temporary, 0o755)?;
    fs::rename(&temporary, destination).context("无法安装 nsetup 二进制")?;
    Ok(())
}

/// 使用明确权限和同目录重命名写入文件。
///
/// # 错误
///
/// 写入、同步或重命名失败时返回错误。
fn write_secure_file(path: &Path, content: &[u8], mode: u32) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("安装路径缺少父目录: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("安装文件名无效: {}", path.display()))?,
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(mode)
        .open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    set_mode(&temporary, mode)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

/// 将已安装文件的所有者和属组改为 `root:nihility`。
///
/// # 错误
///
/// `chown` 执行失败时返回错误。
fn chown_group(path: &Path) -> anyhow::Result<()> {
    run_checked(
        Command::new("chown")
            .arg(format!("root:{ADMIN_GROUP}"))
            .arg(path),
        "设置配置文件属组",
    )
}

/// 执行命令并检查退出状态。
///
/// # 错误
///
/// 进程启动失败或返回非零退出码时返回包含上下文的错误。
fn run_checked(command: &mut Command, operation: &str) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("无法执行 {operation}"))?;
    ensure_success(&output, operation)
}

/// 将失败进程的输出转换为有界诊断信息。
///
/// # 错误
///
/// 进程未成功退出时返回错误。
fn ensure_success(output: &Output, operation: &str) -> anyhow::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "{operation} 失败，退出码 {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

/// 渲染加固后的 systemd unit，并写入全部已配置的可写数据根目录。
fn systemd_unit(config: &Config) -> String {
    let mut writable = vec![PathBuf::from(CONFIG_DIR), config.stacks_root.clone()];
    writable.extend(config.data_roots.iter().cloned());
    writable.sort();
    writable.dedup();
    let write_directives = writable
        .iter()
        .map(|path| format!("ReadWritePaths={}\n", systemd_quote(path)))
        .collect::<String>();
    format!(
        r#"[Unit]
Description=nsetup Docker Compose orchestrator
After=docker.service network-online.target
Wants=network-online.target
Requires=docker.service

[Service]
Type=simple
ExecStart=/usr/local/bin/nsetup daemon
Restart=on-failure
RestartSec=2
User=root
Group=nihility
RuntimeDirectory=nsetup
RuntimeDirectoryMode=0750
UMask=0007
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
{write_directives}

[Install]
WantedBy=multi-user.target
"#
    )
}

/// 为 systemd 指令值引用文件系统路径。
fn systemd_quote(path: &Path) -> String {
    format!(
        "\"{}\"",
        path.display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

#[cfg(test)]
mod tests {
    use super::systemd_unit;
    use crate::config::Config;

    /// 生成的沙箱允许写入自定义项目和数据根目录。
    #[test]
    fn systemd_unit_contains_configured_roots() {
        let config = Config {
            stacks_root: "/mnt/storage/stacks".into(),
            data_roots: vec!["/mnt/storage/data".into()],
            ..Config::default()
        };
        let unit = systemd_unit(&config);
        assert!(unit.contains("ReadWritePaths=\"/mnt/storage/stacks\""));
        assert!(unit.contains("ReadWritePaths=\"/mnt/storage/data\""));
    }
}
