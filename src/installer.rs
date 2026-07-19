//! 单可执行文件系统初始化。

use crate::services::process;
use anyhow::Context;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 单文件安装的可执行文件路径。
const INSTALL_BINARY: &str = "/usr/local/bin/nsetup";
/// 单文件安装的 systemd unit 路径。
const INSTALL_UNIT: &str = "/etc/systemd/system/nsetup.service";
/// 软件包安装的可执行文件路径。
const PACKAGE_BINARY: &str = "/usr/bin/nsetup";
/// 软件包安装的 systemd unit 路径。
const PACKAGE_UNIT: &str = "/lib/systemd/system/nsetup.service";
/// 系统配置文件路径。
const CONFIG_PATH: &str = "/etc/nsetup/config.toml";
/// 拥有本机管理权限的系统组。
const SYSTEM_GROUP: &str = "nihility";
/// 系统文件的用户与组所有权。
const SYSTEM_OWNERSHIP: &str = "root:nihility";
/// 内嵌的默认配置。
const DEFAULT_CONFIG: &str = include_str!("../packaging/config.toml");
/// 内嵌的 systemd unit。
const SYSTEMD_UNIT: &str = include_str!("../packaging/systemd/nsetup.service");

/// 从当前可执行文件初始化系统服务。
pub fn init(force: bool) -> anyhow::Result<()> {
    ensure_root()?;
    ensure_standalone_install()?;
    ensure_command(&["systemctl", "--version"], "检查 systemd")?;
    ensure_command(&["docker", "compose", "version"], "检查 Docker Compose")?;
    preflight(force)?;
    ensure_group()?;
    ensure_directories()?;

    let current_exe = std::env::current_exe().context("无法定位当前 nsetup 可执行文件")?;
    install_copy(&current_exe, Path::new(INSTALL_BINARY), 0o755, force)?;
    install_bytes(
        Path::new(CONFIG_PATH),
        DEFAULT_CONFIG.as_bytes(),
        0o640,
        force,
    )?;
    let unit = render_unit(INSTALL_BINARY);
    install_bytes(Path::new(INSTALL_UNIT), unit.as_bytes(), 0o644, force)?;
    set_group_ownership()?;

    run_status(
        Command::new("systemctl").arg("daemon-reload"),
        "systemctl daemon-reload",
    )?;
    run_status(
        Command::new("systemctl").args(["enable", "nsetup.service"]),
        "启用 nsetup.service",
    )?;
    run_status(
        Command::new("systemctl").args(["restart", "nsetup.service"]),
        "重启 nsetup.service",
    )?;

    tracing::info!("nsetup 已安装到 {INSTALL_BINARY}");
    tracing::info!("配置文件: {CONFIG_PATH}");
    tracing::info!("systemd 服务: nsetup.service");
    tracing::info!("允许用户访问本机服务: sudo usermod -aG nihility <用户名>");
    Ok(())
}

/// 在产生系统变更前检查所有安装目标。
fn preflight(force: bool) -> anyhow::Result<()> {
    for destination in [INSTALL_BINARY, CONFIG_PATH, INSTALL_UNIT] {
        check_destination(Path::new(destination), force)?;
    }
    Ok(())
}

/// 确保当前进程以 root 身份运行。
fn ensure_root() -> anyhow::Result<()> {
    let status = fs::read_to_string("/proc/self/status").context("无法读取当前进程身份")?;
    let effective_uid = parse_effective_uid(&status)
        .ok_or_else(|| anyhow::anyhow!("无法解析当前进程的有效 UID"))?;
    if effective_uid == 0 {
        Ok(())
    } else {
        anyhow::bail!("初始化系统服务需要 root 权限，请使用 sudo ./nsetup init");
    }
}

/// 从 Linux proc 状态内容解析有效 UID。
fn parse_effective_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().nth(1))
        .and_then(|uid| uid.parse().ok())
}

/// 拒绝覆盖由系统包管理器安装的同名服务。
fn ensure_standalone_install() -> anyhow::Result<()> {
    if Path::new(PACKAGE_BINARY).exists() || Path::new(PACKAGE_UNIT).exists() {
        anyhow::bail!(
            "检测到 Debian/RPM 风格的 nsetup 安装；请继续使用包管理器升级，不能执行单文件初始化"
        );
    }
    Ok(())
}

/// 检查初始化依赖的外部命令。
fn ensure_command(args: &[&str], operation: &str) -> anyhow::Result<()> {
    let (program, arguments) = args
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("缺少待执行命令"))?;
    let output = Command::new(program)
        .args(arguments)
        .output()
        .with_context(|| format!("无法执行 {program}"))?;
    process::ensure_success(&output, operation)
}

/// 创建系统组。
fn ensure_group() -> anyhow::Result<()> {
    let exists = Command::new("getent")
        .args(["group", SYSTEM_GROUP])
        .status()
        .context("无法检查 nihility 系统组")?;
    if exists.success() {
        return Ok(());
    }
    run_status(
        Command::new("groupadd").args(["--system", SYSTEM_GROUP]),
        "创建 nihility 系统组",
    )
}

/// 创建配置与状态目录。
fn ensure_directories() -> anyhow::Result<()> {
    for directory in [
        "/etc/nsetup",
        "/var/lib/nsetup",
        "/var/lib/nsetup/stacks",
        "/var/lib/nsetup/data",
    ] {
        let path = Path::new(directory);
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                anyhow::bail!("初始化路径不是普通目录: {}", path.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(path)
                    .with_context(|| format!("无法创建初始化目录: {}", path.display()))?;
            }
            Err(error) => return Err(error.into()),
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
    }
    Ok(())
}

/// 设置配置和状态目录的组所有权。
fn set_group_ownership() -> anyhow::Result<()> {
    run_status(
        Command::new("chown").args([
            SYSTEM_OWNERSHIP,
            "/etc/nsetup",
            "/etc/nsetup/config.toml",
            "/var/lib/nsetup",
            "/var/lib/nsetup/stacks",
            "/var/lib/nsetup/data",
        ]),
        "设置 nsetup 目录所有权",
    )
}

/// 原子复制当前可执行文件。
fn install_copy(source: &Path, destination: &Path, mode: u32, force: bool) -> anyhow::Result<()> {
    check_destination(destination, force)?;
    let temporary = temporary_path(destination)?;
    fs::copy(source, &temporary).with_context(|| {
        format!(
            "无法复制可执行文件 {} -> {}",
            source.display(),
            temporary.display()
        )
    })?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    fs::rename(&temporary, destination)
        .with_context(|| format!("无法安装文件: {}", destination.display()))
}

/// 原子写入内嵌资源。
fn install_bytes(destination: &Path, content: &[u8], mode: u32, force: bool) -> anyhow::Result<()> {
    check_destination(destination, force)?;
    let temporary = temporary_path(destination)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(mode);
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("无法创建临时安装文件: {}", temporary.display()))?;
    file.write_all(content)?;
    file.sync_all()?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    fs::rename(&temporary, destination)
        .with_context(|| format!("无法安装文件: {}", destination.display()))
}

/// 校验目标文件，避免覆盖符号链接或意外文件。
fn check_destination(destination: &Path, force: bool) -> anyhow::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            anyhow::bail!("拒绝覆盖非普通文件: {}", destination.display());
        }
        Ok(_) if !force => {
            anyhow::bail!(
                "目标文件已存在: {}；确认覆盖请使用 --force",
                destination.display()
            );
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// 构造与目标文件同目录的临时路径。
fn temporary_path(destination: &Path) -> anyhow::Result<PathBuf> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("无效安装路径: {}", destination.display()))?;
    Ok(destination.with_file_name(format!(
        ".{name}.tmp-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    )))
}

/// 将包模板中的二进制路径替换为单文件安装路径。
fn render_unit(binary: &str) -> String {
    SYSTEMD_UNIT.replace("/usr/bin/nsetup", binary)
}

/// 执行不捕获输出的命令并检查退出状态。
fn run_status(command: &mut Command, operation: &str) -> anyhow::Result<()> {
    let status = command
        .status()
        .with_context(|| format!("无法执行 {operation}"))?;
    process::ensure_status(status, operation)
}
