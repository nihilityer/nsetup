//! daemon 配置加载与校验。

use crate::constants::{GRPC_SOCKET, config_path};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::Permissions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// 存储在 `/etc/nsetup/config.toml` 中的扁平 daemon 配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 短路由主机名拼接的基础域名。
    pub domain: String,
    /// Compose 项目根目录，每个项目对应一个子目录。
    pub stacks_root: PathBuf,
    /// bind mount 源路径允许使用的根目录。
    pub data_roots: Vec<PathBuf>,
    /// gRPC 监听地址，可为 `unix:///path` 或 TCP 地址。
    pub listen: String,
    /// Docker daemon 的 Unix socket。
    pub docker_socket: PathBuf,
}

impl Config {
    /// 返回安全的系统默认配置。
    #[must_use]
    pub fn default_system() -> Self {
        Self {
            domain: String::from("example.com"),
            stacks_root: PathBuf::from("/var/lib/nsetup/stacks"),
            data_roots: vec![PathBuf::from("/var/lib/nsetup/data")],
            listen: format!("unix://{GRPC_SOCKET}"),
            docker_socket: PathBuf::from("/var/run/docker.sock"),
        }
    }

    /// 解析并校验 TOML 配置。
    ///
    /// # 错误
    ///
    /// TOML 格式错误或配置值不安全时返回错误。
    pub fn from_toml(input: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(input).context("配置文件格式错误")?;
        config.validate()?;
        Ok(config)
    }

    /// 加载系统配置文件。
    ///
    /// # 错误
    ///
    /// 文件无法读取或未通过校验时返回错误。
    pub fn load() -> anyhow::Result<Self> {
        let path = config_path();
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("无法读取配置文件: {}", path.display()))?;
        Self::from_toml(&content).with_context(|| format!("配置文件无效: {}", path.display()))
    }

    /// 加载现有配置；文件不存在时返回系统默认配置。
    ///
    /// # 错误
    ///
    /// 现有文件无法读取或内容无效时返回错误。
    pub fn load_or_default() -> anyhow::Result<Self> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => Self::from_toml(&content)
                .with_context(|| format!("配置文件无效: {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default_system())
            }
            Err(error) => {
                Err(error).with_context(|| format!("无法读取配置文件: {}", path.display()))
            }
        }
    }

    /// 校验全部配置值和跨字段约束。
    ///
    /// # 错误
    ///
    /// 域名、路径或监听地址无效时返回错误。
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_domain(&self.domain)?;
        validate_absolute("stacks_root", &self.stacks_root)?;
        validate_absolute("docker_socket", &self.docker_socket)?;
        if self.data_roots.is_empty() {
            anyhow::bail!("data_roots 至少需要一个路径");
        }
        let mut roots = BTreeSet::new();
        for root in &self.data_roots {
            validate_absolute("data_roots", root)?;
            if !roots.insert(root) {
                anyhow::bail!("data_roots 路径重复: {}", root.display());
            }
        }
        if let Some(socket) = self.listen.strip_prefix("unix://") {
            validate_absolute("listen", Path::new(socket))?;
        } else {
            let address = self.listen.strip_prefix("tcp://").unwrap_or(&self.listen);
            address
                .parse::<std::net::SocketAddr>()
                .map_err(|error| anyhow::anyhow!("listen 不是有效地址: {error}"))?;
        }
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::default_system()
    }
}

/// 设置路径的 Unix 权限位。
///
/// # 错误
///
/// 无法修改权限时返回错误。
pub fn set_mode(path: &Path, mode: u32) -> anyhow::Result<()> {
    std::fs::set_permissions(path, Permissions::from_mode(mode))
        .with_context(|| format!("无法设置路径权限: {}", path.display()))
}

/// 原子替换系统配置文件并保持 `root:nihility` 属组。
///
/// # 错误
///
/// 写入、同步、授权或重命名失败时返回错误。
pub fn write_system_config(config: &Config) -> anyhow::Result<()> {
    let path = config_path();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("配置路径缺少父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_file_name(format!(".config.toml.tmp-{}", std::process::id()));
    let content = toml::to_string_pretty(config)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o640)
        .open(&temporary)
        .with_context(|| format!("无法写入配置文件: {}", temporary.display()))?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    drop(file);
    set_mode(&temporary, 0o640)?;
    let status = std::process::Command::new("chown")
        .arg(format!("root:{}", crate::constants::ADMIN_GROUP))
        .arg(&temporary)
        .status()
        .context("无法执行 chown 设置配置文件属组")?;
    if !status.success() {
        let _removed = std::fs::remove_file(&temporary);
        anyhow::bail!("无法设置配置文件属组，请以 root 运行");
    }
    std::fs::rename(&temporary, &path)
        .with_context(|| format!("无法替换配置文件: {}", path.display()))?;
    Ok(())
}

/// 校验路由与基础域名配置接受的 DNS 名称。
///
/// # 错误
///
/// 输入不符合保守的 DNS 名称规则时返回错误。
pub fn validate_domain(domain: &str) -> anyhow::Result<()> {
    let domain = domain.trim_end_matches('.');
    if domain.is_empty() || domain.len() > 253 {
        anyhow::bail!("域名长度无效: {domain}");
    }
    for label in domain.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            anyhow::bail!("域名无效: {domain}");
        }
    }
    Ok(())
}

/// 确保配置路径是绝对路径。
fn validate_absolute(label: &str, path: &Path) -> anyhow::Result<()> {
    if !path.is_absolute() {
        anyhow::bail!("{label} 必须是绝对路径: {}", path.display());
    }
    if path == Path::new("/") {
        anyhow::bail!("{label} 不能是文件系统根目录");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn parses_flat_configuration() -> anyhow::Result<()> {
        let config = Config::from_toml(
            r#"
domain = "example.com"
stacks_root = "/srv/stacks"
data_roots = ["/srv/data"]
listen = "unix:///run/nsetup.sock"
docker_socket = "/var/run/docker.sock"
"#,
        )?;
        assert_eq!(config.domain, "example.com");
        Ok(())
    }

    /// 未知扁平键必须报错，不能静默忽略。
    #[test]
    fn rejects_unknown_configuration_keys() {
        let result = Config::from_toml(
            r#"
domain = "example.com"
stacks_root = "/srv/stacks"
data_roots = ["/srv/data"]
listen = "unix:///run/nsetup.sock"
docker_socket = "/var/run/docker.sock"
unexpected = true
"#,
        );
        assert!(result.is_err());
    }

    /// 根目录白名单会使挂载限制失效，因此必须拒绝。
    #[test]
    fn rejects_filesystem_root_as_data_root() {
        let config = Config {
            data_roots: vec!["/".into()],
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
