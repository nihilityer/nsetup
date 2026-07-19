use crate::constants::*;
use anyhow::Context;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::fs::Permissions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// `nsetup` 全局配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 路径配置
    pub paths: PathsConfig,
    /// 域名配置
    pub home: HomeConfig,
    /// gRPC 守护进程配置
    #[serde(default)]
    pub grpc: GrpcConfig,
}

/// 路径配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    /// Compose 项目根目录
    #[serde(rename = "stacks_root", alias = "apps_root")]
    pub apps_root: PathBuf,
    /// 数据根目录
    pub data_root: PathBuf,
}

/// gRPC 守护进程配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrpcConfig {
    /// 服务监听地址；默认只监听本机，避免暴露 Docker 管理权限
    pub listen: String,
}

impl Default for GrpcConfig {
    fn default() -> Self {
        Self {
            listen: format!("unix://{GRPC_SOCKET}"),
        }
    }
}

/// 域名配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeConfig {
    /// 主域名
    pub domain: String,
}

impl Config {
    /// 创建符合 Linux FHS 的系统服务默认配置
    pub fn default_system() -> Self {
        Self {
            paths: PathsConfig {
                apps_root: PathBuf::from(SYSTEM_STATE_DIR).join(STACKS_DIR),
                data_root: PathBuf::from(SYSTEM_STATE_DIR).join("data"),
            },
            home: HomeConfig {
                domain: String::from("example.com"),
            },
            grpc: GrpcConfig::default(),
        }
    }

    /// 加载配置并保留文件读取或解析错误
    pub fn load_checked() -> anyhow::Result<Option<Self>> {
        let path = config_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("无法读取配置文件: {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("配置文件格式错误: {}", path.display()))?;
        config.validate()?;
        Ok(Some(config))
    }

    /// 加载或创建默认配置
    pub fn load_or_default() -> anyhow::Result<Self> {
        if let Some(cfg) = Self::load_checked()? {
            return Ok(cfg);
        }
        // 返回一个未初始化的配置，由 init 命令填写
        Ok(Self::default_system())
    }

    /// 校验域名、路径和监听地址，避免 daemon 使用含糊或临时配置。
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_domain(&self.home.domain)?;
        for (label, path) in [
            ("stacks_root", &self.paths.apps_root),
            ("data_root", &self.paths.data_root),
        ] {
            if !path.is_absolute() {
                anyhow::bail!("{label} 必须是绝对路径: {}", path.display());
            }
            if path == std::path::Path::new("/") {
                anyhow::bail!("{label} 不能使用文件系统根目录");
            }
        }
        if self.paths.apps_root == self.paths.data_root {
            anyhow::bail!("stacks_root 和 data_root 不能指向同一目录");
        }
        if self.grpc.listen.trim().is_empty() {
            anyhow::bail!("grpc.listen 不能为空");
        }
        Ok(())
    }
}

/// 校验可用于应用子域名拼接的主域名。
pub fn validate_domain(domain: &str) -> anyhow::Result<()> {
    let valid = !domain.is_empty()
        && domain.len() <= 253
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        });
    if !valid {
        anyhow::bail!("domain 必须是有效的全小写完整域名");
    }
    Ok(())
}

/// 获取配置文件路径
pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

/// 获取系统配置目录；测试和开发可通过环境变量覆盖
pub fn config_dir() -> PathBuf {
    std::env::var("NSETUP_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(SYSTEM_CONFIG_DIR))
}

/// 创建或读取 gRPC 认证令牌
pub fn ensure_auth_token() -> anyhow::Result<String> {
    ensure_auth_token_in(&config_dir())
}

/// 在指定配置目录创建或读取 gRPC 认证令牌
pub fn ensure_auth_token_in(directory: &std::path::Path) -> anyhow::Result<String> {
    let path = directory.join(AUTH_TOKEN_FILE);
    if path.exists() {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            anyhow::bail!("认证令牌路径必须是普通文件: {}", path.display());
        }
        set_mode(&path, 0o600)?;
        let token = std::fs::read_to_string(&path)
            .map(|value| value.trim().to_string())
            .with_context(|| format!("无法读取认证令牌: {}", path.display()))?;
        if token.len() < 32 {
            anyhow::bail!("认证令牌无效或过短: {}", path.display());
        }
        return Ok(token);
    }

    ensure_regular_directory(directory, 0o700)?;
    set_mode(directory, 0o700)?;
    let charset = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    let token: String = (0..64)
        .map(|_| charset[rng.random_range(0..charset.len())] as char)
        .collect();
    write_private(&path, format!("{token}\n").as_bytes())?;
    set_mode(directory, 0o700)?;
    set_mode(&path, 0o600)?;
    Ok(token)
}

/// 以仅所有者可读写的权限写入敏感文件
fn write_private(path: &std::path::Path, content: &[u8]) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            anyhow::bail!("敏感文件路径必须是普通文件: {}", path.display());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("无法写入敏感文件: {}", path.display()))?;
    set_mode(path, 0o600)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

/// 创建普通目录，并拒绝跟随符号链接
fn ensure_regular_directory(path: &std::path::Path, mode: u32) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                anyhow::bail!("配置路径必须是普通目录: {}", path.display());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)
                .with_context(|| format!("无法创建目录: {}", path.display()))?;
        }
        Err(error) => {
            return Err(error.into());
        }
    }
    set_mode(path, mode)
}

/// 设置文件或目录的 Unix 权限
pub fn set_mode(path: &std::path::Path, mode: u32) -> anyhow::Result<()> {
    std::fs::set_permissions(path, Permissions::from_mode(mode))
        .with_context(|| format!("无法设置权限 {:o}: {}", mode, path.display()))
}

/// 检查 docker 是否可用
pub fn check_docker() -> anyhow::Result<()> {
    let output = std::process::Command::new("docker")
        .arg("info")
        .output()
        .context("Docker 未安装或不可用")?;
    if !output.status.success() {
        anyhow::bail!("Docker daemon 未运行或权限不足");
    }
    Ok(())
}
