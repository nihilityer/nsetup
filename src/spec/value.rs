//! 端口、挂载、健康检查、镜像与名称值对象。

use super::{BindMount, Healthcheck, PortProtocol, PublishedPort, Route};
use crate::config::validate_domain;
use anyhow::Context;
use std::path::Path;

impl Route {
    /// 校验语义路由。
    ///
    /// # 错误
    ///
    /// 缺少主机名，或 DNS 名称、路径、端口无效时返回错误。
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_name("Traefik 路由名", &self.name)?;
        if self.hosts.is_empty() {
            anyhow::bail!("Traefik 路由至少需要一个 host");
        }
        for host in &self.hosts {
            validate_domain(host)?;
        }
        if let Some(path) = &self.path_prefix
            && (!path.starts_with('/') || path.contains('`'))
        {
            anyhow::bail!("path_prefix 必须以 / 开头且不能含反引号: {path}");
        }
        if self.container_port == 0 {
            anyhow::bail!("路由容器端口必须在 1..=65535 范围内");
        }
        Ok(())
    }
}

impl PublishedPort {
    /// 解析 Docker Compose 短端口语法。
    ///
    /// # 错误
    ///
    /// 映射不受支持或超出范围时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let (mapping, protocol) = match value.rsplit_once('/') {
            Some((mapping, "tcp")) => (mapping, PortProtocol::Tcp),
            Some((mapping, "udp")) => (mapping, PortProtocol::Udp),
            Some((_mapping, protocol)) => anyhow::bail!("不支持的端口协议: {protocol}"),
            None => (value, PortProtocol::Tcp),
        };
        let parts: Vec<&str> = mapping.split(':').collect();
        let (host_ip, host, container) = match parts.as_slice() {
            [host, container] => (None, *host, *container),
            [host_ip, host, container] => (Some((*host_ip).to_string()), *host, *container),
            _ => anyhow::bail!("端口映射必须是 HOST:CONTAINER[/tcp|udp]: {value}"),
        };
        let host_port = parse_port(host, "宿主机端口")?;
        let container_port = parse_port(container, "容器端口")?;
        Ok(Self {
            host_ip,
            host_port,
            container_port,
            protocol,
        })
    }

    /// 将映射序列化为规范短语法。
    #[must_use]
    pub fn compose_value(&self) -> String {
        let address = self
            .host_ip
            .as_ref()
            .map(|value| format!("{value}:"))
            .unwrap_or_default();
        format!(
            "{address}{}:{}/{}",
            self.host_port,
            self.container_port,
            self.protocol.as_str()
        )
    }
}

impl BindMount {
    /// 解析 bind mount 并拒绝命名卷。
    ///
    /// # 错误
    ///
    /// 遇到命名卷、相对路径或无效目标时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = value.split(':').collect();
        let (host_path, container_path, read_only) = match parts.as_slice() {
            [host, container] => (*host, *container, false),
            [host, container, "ro"] => (*host, *container, true),
            [_, _, mode] => anyhow::bail!("卷挂载模式仅支持 ro: {mode}"),
            _ => anyhow::bail!("卷挂载格式必须是 HOST:CONTAINER[:ro]: {value}"),
        };
        if !Path::new(host_path).is_absolute() {
            anyhow::bail!("禁止命名卷或相对 bind mount: {host_path}");
        }
        if !Path::new(container_path).is_absolute() {
            anyhow::bail!("容器挂载目标必须是绝对路径: {container_path}");
        }
        Ok(Self {
            host_path: host_path.to_string(),
            container_path: container_path.to_string(),
            read_only,
        })
    }

    /// 将 bind mount 序列化为规范短语法。
    #[must_use]
    pub fn compose_value(&self) -> String {
        format!(
            "{}:{}{}",
            self.host_path,
            self.container_path,
            if self.read_only { ":ro" } else { "" }
        )
    }
}

impl Healthcheck {
    /// 构造 `CMD-SHELL` 健康检查。
    #[must_use]
    pub fn command(command: String) -> Self {
        Self {
            test: vec![String::from("CMD-SHELL"), command],
            interval: None,
            timeout: None,
            start_period: None,
            retries: None,
        }
    }

    /// 当前健康检查受支持时返回 shell 命令。
    ///
    /// # 错误
    ///
    /// 健康检查不是 `CMD-SHELL` 形式时返回错误。
    pub fn shell_command(&self) -> anyhow::Result<&str> {
        match self.test.as_slice() {
            [kind, command] if kind == "CMD-SHELL" && !command.is_empty() => Ok(command),
            _ => anyhow::bail!("healthcheck.test 只支持 [CMD-SHELL, command]"),
        }
    }

    /// 校验受支持的健康检查表示。
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        let _command = self.shell_command()?;
        if self.retries == Some(0) {
            anyhow::bail!("healthcheck.retries 必须大于 0");
        }
        Ok(())
    }
}

/// 校验受管项目名或服务名。
///
/// # 错误
///
/// 违反保守命名规则时返回错误。
pub fn validate_name(label: &str, value: &str) -> anyhow::Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 63
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte)
        });
    if !valid {
        anyhow::bail!("{label}无效: {value}（需小写字母开头，仅含小写字母、数字、-、_，最长 63）");
    }
    Ok(())
}

/// 校验明确的镜像版本。
///
/// # 错误
///
/// 标签为 `latest`、为空、过长或格式无效时返回错误。
pub fn validate_version(version: &str) -> anyhow::Result<()> {
    let valid = !version.is_empty()
        && version != "latest"
        && version.len() <= 128
        && (version.as_bytes()[0].is_ascii_alphanumeric() || version.as_bytes()[0] == b'_')
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        anyhow::bail!("镜像版本无效或未钉定: {version}");
    }
    Ok(())
}

/// 拆分包含完整标签的镜像引用。
///
/// # 错误
///
/// 缺少明确标签或标签无效时返回错误。
pub fn split_tagged_image(image: &str) -> anyhow::Result<(String, String)> {
    if image.contains('@') {
        anyhow::bail!("镜像必须使用显式版本标签，不能使用摘要: {image}");
    }
    let slash = image.rfind('/');
    let colon = image
        .rfind(':')
        .filter(|index| slash.is_none_or(|slash_index| *index > slash_index))
        .ok_or_else(|| anyhow::anyhow!("镜像缺少显式版本标签: {image}"))?;
    let repository = &image[..colon];
    let version = &image[colon + 1..];
    if repository.is_empty() || repository.chars().any(char::is_whitespace) {
        anyhow::bail!("镜像仓库无效: {repository}");
    }
    validate_version(version)?;
    Ok((repository.to_string(), version.to_string()))
}

/// 校验包含完整标签的镜像，并丢弃拆分结果。
pub(super) fn validate_tagged_image(image: &str) -> anyhow::Result<()> {
    let _parts = split_tagged_image(image)?;
    Ok(())
}

/// 解析非零 TCP 或 UDP 端口号并检查范围。
fn parse_port(value: &str, label: &str) -> anyhow::Result<u16> {
    let port = value
        .parse::<u16>()
        .with_context(|| format!("{label}无效: {value}"))?;
    if port == 0 {
        anyhow::bail!("{label}不能为 0");
    }
    Ok(port)
}

/// 要求环境文件路径保持在项目目录内。
pub(super) fn validate_relative_compose_path(value: &str) -> anyhow::Result<()> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        anyhow::bail!("env_file 必须是项目内的安全相对路径: {value}");
    }
    Ok(())
}

/// 校验直接交给 Docker、而非使用项目命名规则的名称。
pub(super) fn validate_docker_object_name(label: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 255
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("{label}无效: {value}");
    }
    Ok(())
}
