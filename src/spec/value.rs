//! 端口、挂载、健康检查、镜像与名称值对象。

use super::{BindMount, Healthcheck, PortProtocol, PublishedPort, Route, ServiceHooks};
use crate::config::validate_domain;
use anyhow::Context;
use std::path::Path;

impl Route {
    /// 校验语义路由。
    ///
    /// # 错误
    ///
    /// 缺少主机名，或 DNS 名称、路径、入口、端口无效时返回错误。
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
        if !self.entrypoint.trim().is_empty() {
            let _entrypoints = validate_entrypoints(&self.entrypoint)?;
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
    /// 宿主机侧支持三种写法：以 `/` 开头的绝对路径、以 `./` 开头或直接以名字开头的
    /// 项目内相对路径（例如 `files/prometheus/prometheus.yml`）。相对路径由 daemon
    /// 按受管项目目录解析并校验，因此 `stacks_root` 变更时不需要修改仓库里的 TOML。
    ///
    /// # 错误
    ///
    /// 遇到命名卷、无效相对路径或无效目标时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = value.split(':').collect();
        let (host_path, container_path, read_only) = match parts.as_slice() {
            [host, container] => (*host, *container, false),
            [host, container, "ro"] => (*host, *container, true),
            [_, _, mode] => anyhow::bail!("卷挂载模式仅支持 ro: {mode}"),
            _ => anyhow::bail!("卷挂载格式必须是 HOST:CONTAINER[:ro]: {value}"),
        };
        let host_path = host_path.trim();
        if host_path.starts_with('/') {
            // 绝对路径：继续由部署期的白名单校验，见 Orchestrator::validate_mounts。
        } else if is_project_relative_source(host_path) {
            validate_project_relative_source(host_path)?;
        } else {
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

    /// 判断宿主机源是否为相对受管项目目录的路径。
    #[must_use]
    pub fn is_project_relative(&self) -> bool {
        !self.host_path.starts_with('/')
    }

    /// 将相对项目目录的源解析为绝对路径；绝对路径原样返回。
    #[must_use]
    pub fn resolved_host_path(&self, project_directory: &Path) -> std::path::PathBuf {
        if self.is_project_relative() {
            project_directory.join(&self.host_path)
        } else {
            std::path::PathBuf::from(&self.host_path)
        }
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

/// 判断宿主机源是否像项目内相对路径，而不是命名卷。
///
/// `./x`、`../x` 是明确的相对路径；没有 `.` 前缀时要求至少含一个 `/`，从而把
/// `mydata` 这类命名卷与 `files/x.yaml` 这类相对路径区分开。
fn is_project_relative_source(value: &str) -> bool {
    value.starts_with("./") || value.starts_with("../") || value.contains('/')
}

/// 校验项目内相对挂载源不越出项目目录。
///
/// # 错误
///
/// 路径为空、指向上级目录或含非法分量时返回错误。
fn validate_project_relative_source(value: &str) -> anyhow::Result<()> {
    let path = Path::new(value);
    if path.as_os_str().is_empty() {
        anyhow::bail!("bind mount 宿主机源不能为空");
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => {}
            std::path::Component::CurDir => {}
            _ => anyhow::bail!(
                "相对 bind mount 必须是项目目录内的路径，不能包含 ..、根或前缀: {value}"
            ),
        }
    }
    Ok(())
}

/// 归一化绝对路径，移除 `.` 与多余的 `..` 分量。
///
/// # 错误
///
/// 路径不是绝对路径或 `..` 越出根目录时返回错误。
pub(super) fn normalize_absolute_path(path: &Path) -> anyhow::Result<std::path::PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("路径必须是绝对路径: {}", path.display());
    }
    let mut output = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir => output.push(Path::new("/")),
            std::path::Component::Normal(value) => output.push(value),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !output.pop() {
                    anyhow::bail!("路径越出根目录: {}", path.display());
                }
            }
            std::path::Component::Prefix(_) => {
                anyhow::bail!("不支持的平台路径: {}", path.display());
            }
        }
    }
    Ok(output)
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

    /// 构造 exec 形式的 `CMD` 健康检查。
    ///
    /// 没有 shell 的镜像只能用这种形式；等价于 Dockerfile 的 `HEALTHCHECK CMD`。
    ///
    /// # 错误
    ///
    /// 参数列表为空时返回错误。
    pub fn exec(arguments: &[String]) -> anyhow::Result<Self> {
        if arguments.is_empty() {
            anyhow::bail!("CMD 健康检查至少需要一个参数");
        }
        let mut test = vec![String::from("CMD")];
        test.extend(arguments.iter().cloned());
        Ok(Self {
            test,
            interval: None,
            timeout: None,
            start_period: None,
            retries: None,
        })
    }

    /// 当前健康检查为 `CMD-SHELL` 形式时返回 shell 命令。
    ///
    /// # 错误
    ///
    /// 健康检查不是 `CMD-SHELL` 形式时返回错误。
    pub fn shell_command(&self) -> anyhow::Result<&str> {
        match self.test.as_slice() {
            [kind, command] if kind == "CMD-SHELL" && !command.is_empty() => Ok(command),
            _ => anyhow::bail!("healthcheck.test 不是 [CMD-SHELL, command] 形式"),
        }
    }

    /// 当前健康检查为 exec 形式时返回参数列表。
    ///
    /// # 错误
    ///
    /// 健康检查不是 `CMD` 形式时返回错误。
    pub fn exec_arguments(&self) -> anyhow::Result<&[String]> {
        match self.test.split_first() {
            Some((kind, arguments)) if kind == "CMD" && !arguments.is_empty() => Ok(arguments),
            _ => anyhow::bail!("healthcheck.test 不是 [CMD, ..] 形式"),
        }
    }

    /// 校验受支持的健康检查表示。
    ///
    /// # 错误
    ///
    /// 测试命令不受支持或重试次数为 0 时返回错误。
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.test.len() > 64 {
            anyhow::bail!("healthcheck.test 参数过多");
        }
        let shell = self.shell_command().is_ok();
        let exec = match self.exec_arguments() {
            Ok(arguments) => {
                arguments.iter().all(|value| !value.is_empty())
                    && arguments
                        .iter()
                        .all(|value| !value.contains(['\n', '\r', '\0']))
            }
            Err(_) => false,
        };
        if !shell && !exec {
            anyhow::bail!("healthcheck.test 只支持 [CMD-SHELL, command] 或 [CMD, arg, ..]");
        }
        if let Ok(command) = self.shell_command()
            && command.contains('\0')
        {
            anyhow::bail!("healthcheck.test 命令不能包含空字符");
        }
        if self.retries == Some(0) {
            anyhow::bail!("healthcheck.retries 必须大于 0");
        }
        Ok(())
    }
}

/// 校验容器运行用户覆盖值。
///
/// # 错误
///
/// 值不是 `UID[:GID]` 形式的数字时返回错误。
pub fn validate_user(value: &str) -> anyhow::Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && !value.contains(char::is_whitespace)
        && value.split(':').count() <= 2
        && value
            .split(':')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if !valid {
        anyhow::bail!("user 必须是 UID[:GID] 形式的数字: {value}");
    }
    Ok(())
}

/// 校验补充用户组条目。
///
/// # 错误
///
/// 组名称为空、含控制字符或过长时返回错误。
pub fn validate_group(value: &str) -> anyhow::Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && !value.contains(char::is_whitespace)
        && !value.chars().any(char::is_control);
    if !valid {
        anyhow::bail!("group_add 条目无效: {value}");
    }
    Ok(())
}

/// 校验服务启动钩子命令。
///
/// # 错误
///
/// 命令为空、过长或含空字符时返回错误。
pub fn validate_hooks(label: &str, hooks: &ServiceHooks) -> anyhow::Result<()> {
    for stage in [super::HookStage::PreStart, super::HookStage::PostStart] {
        for command in hooks.commands(stage) {
            if command.trim().is_empty() {
                anyhow::bail!("{label} 的 hooks.{} 命令不能为空", stage.label());
            }
            if command.len() > 4096 {
                anyhow::bail!("{label} 的 hooks.{} 命令过长", stage.label());
            }
            if command.contains('\0') {
                anyhow::bail!("{label} 的 hooks.{} 命令不能包含空字符", stage.label());
            }
        }
    }
    Ok(())
}

/// 校验自定义 Traefik 中间件名称。
///
/// 中间件名可以携带 `@file`、`@docker` 等 provider 后缀；裸名称默认指向
/// `@file`，与内置中间件保持一致。
///
/// # 错误
///
/// 名称为空或包含无法出现在 Traefik label 中的字符时返回错误。
pub fn validate_middleware(value: &str) -> anyhow::Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'));
    if !valid {
        anyhow::bail!("Traefik 中间件名无效: {value}");
    }
    Ok(())
}

/// 校验 Traefik entrypoint 列表。
///
/// # 错误
///
/// 列表为空或含非法名称时返回错误。
pub fn validate_entrypoints(value: &str) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    for name in value.split(',') {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            anyhow::bail!("Traefik entrypoint 无效: {value}");
        }
        names.push(name.to_string());
    }
    if names.is_empty() {
        anyhow::bail!("Traefik entrypoint 不能为空: {value}");
    }
    Ok(names)
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

#[cfg(test)]
mod tests {
    use super::BindMount;

    /// 挂载源支持绝对路径与相对项目目录两种写法（R6）。
    #[test]
    fn bind_mount_accepts_project_relative_sources() -> anyhow::Result<()> {
        let relative =
            BindMount::parse("files/prometheus/prometheus.yml:/etc/prometheus/prometheus.yml:ro")?;
        assert!(relative.is_project_relative());
        assert!(relative.read_only);
        assert_eq!(
            relative.resolved_host_path(std::path::Path::new("/srv/stacks/demo")),
            std::path::Path::new("/srv/stacks/demo/files/prometheus/prometheus.yml")
        );

        let dotted = BindMount::parse("./files:/opt/files:ro")?;
        assert!(dotted.is_project_relative());

        let absolute = BindMount::parse("/srv/data/demo:/data")?;
        assert!(!absolute.is_project_relative());
        assert_eq!(
            absolute.resolved_host_path(std::path::Path::new("/srv/stacks/demo")),
            std::path::Path::new("/srv/data/demo")
        );
        Ok(())
    }

    /// 命名卷与越出项目目录的相对路径仍然被拒绝。
    #[test]
    fn bind_mount_rejects_named_volumes_and_escapes() {
        assert!(BindMount::parse("mydata:/data").is_err());
        assert!(BindMount::parse("../outside:/data").is_err());
        assert!(BindMount::parse("files/../../etc:/data").is_err());
        assert!(BindMount::parse("./files:relative/target").is_err());
    }

    /// 绝对路径分量被归一化，去掉多余的 `.`（R6）。
    #[test]
    fn normalizes_absolute_paths() -> anyhow::Result<()> {
        assert_eq!(
            super::normalize_absolute_path(std::path::Path::new("/srv/stacks/demo/./files"))?,
            std::path::Path::new("/srv/stacks/demo/files")
        );
        assert_eq!(
            super::normalize_absolute_path(std::path::Path::new("/srv/one/../two"))?,
            std::path::Path::new("/srv/two")
        );
        assert!(super::normalize_absolute_path(std::path::Path::new("relative")).is_err());
        Ok(())
    }
}
