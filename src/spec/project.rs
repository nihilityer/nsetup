//! 项目级 Compose 状态加载、校验与序列化。

use super::service::{label_map, parse_hosts, validate_labels};
use super::value::{
    validate_docker_object_name, validate_group, validate_relative_compose_path,
    validate_tagged_image, validate_user,
};
use super::{
    BindMount, Document, PublishedPort, RouteBinding, ServiceHooks, StackSpec, validate_name,
};
use crate::config::validate_domain;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

impl StackSpec {
    /// 将 Compose YAML 和可选环境文件内容解析为 IR。
    ///
    /// # 错误
    ///
    /// 字段不受支持或状态无效时返回包含上下文的错误。
    pub fn parse(name: &str, compose_yaml: &str, env_file: &str) -> anyhow::Result<Self> {
        validate_name("项目名", name)?;
        let document: Document =
            serde_yaml::from_str(compose_yaml).context("Compose YAML 含不支持的字段或值")?;
        let spec = Self {
            name: name.to_string(),
            document,
            environment: parse_env_file(env_file)?,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// 用已解析的 Compose 文档与项目环境构造并校验状态。
    ///
    /// 供宽容导入路径复用：YAML 由调用方解析并剔除不支持的键。
    ///
    /// # 错误
    ///
    /// 项目名、镜像、端口、label 或挂载无效时返回错误。
    pub fn parse_compose(name: &str, document: Document, env_file: &str) -> anyhow::Result<Self> {
        validate_name("项目名", name)?;
        let spec = Self {
            name: name.to_string(),
            document,
            environment: parse_env_file(env_file)?,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// 从项目目录加载 `compose.yaml` 和 `.env`。
    ///
    /// # 错误
    ///
    /// 文件缺失、无法读取或内容无效时返回错误。
    pub fn load(directory: &Path) -> anyhow::Result<Self> {
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("无法从目录取得项目名: {}", directory.display()))?;
        let compose = std::fs::read_to_string(directory.join(COMPOSE_FILE))
            .with_context(|| format!("无法读取 {COMPOSE_FILE}: {}", directory.display()))?;
        let env = match std::fs::read_to_string(directory.join(ENV_FILE)) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        Self::parse(name, &compose, &env)
    }

    /// 校验 IR 表示的名称、镜像、端口、label 和挂载。
    ///
    /// # 错误
    ///
    /// 状态无法安全管理和重新生成时返回错误。
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_name("项目名", &self.name)?;
        if self.document.services.is_empty() {
            anyhow::bail!("Compose 至少需要一个服务");
        }
        for (name, network) in &self.document.networks {
            validate_docker_object_name("Compose 网络名", name)?;
            if let Some(docker_name) = &network.name {
                validate_docker_object_name("Docker 网络名", docker_name)?;
            }
        }
        for (name, service) in &self.document.services {
            validate_name("服务名", name)?;
            if let Some(container_name) = &service.container_name {
                validate_docker_object_name("container_name", container_name)?;
            }
            if let Some(user) = &service.user {
                validate_user(user)?;
            }
            for group in &service.group_add {
                validate_group(group)?;
            }
            let resolved = interpolate_image(&service.image, &self.environment)?;
            validate_tagged_image(&resolved)?;
            if service
                .network_mode
                .as_deref()
                .is_some_and(|mode| mode != "host")
            {
                anyhow::bail!("服务 {name} 使用了不支持的 network_mode");
            }
            for network in &service.networks {
                if !self.document.networks.contains_key(network) {
                    anyhow::bail!("服务 {name} 引用了未定义网络 {network}");
                }
            }
            let mut ports = BTreeSet::new();
            for value in &service.ports {
                let port = PublishedPort::parse(value)?;
                if !ports.insert((port.host_ip.clone(), port.host_port, port.protocol)) {
                    anyhow::bail!("服务 {name} 重复发布宿主机端口 {value}");
                }
            }
            for value in &service.volumes {
                let _mount = BindMount::parse(value)?;
            }
            for env_file in &service.env_file {
                validate_relative_compose_path(env_file)?;
            }
            validate_labels(&service.labels)?;
            if let Some(healthcheck) = &service.healthcheck {
                healthcheck.validate()?;
            }
            if let Some(logging) = &service.logging
                && logging.driver.trim().is_empty()
            {
                anyhow::bail!("服务 {name} 的 logging.driver 不能为空");
            }
            for route in service.routes(&self.name, name)? {
                route.validate()?;
            }
        }
        let _bindings = self.route_bindings()?;
        let _hooks = self.project_hooks()?;
        Ok(())
    }

    /// 把相对受管项目目录的 bind mount 源展开为绝对路径。
    ///
    /// Compose 只把 `./`、`../` 或绝对路径当作宿主路径，其余短语法一律按命名卷处理；
    /// 因此 TOML 里允许的 `files/x.yaml` 这类写法必须在生成 Compose 之前展开为
    /// 项目目录下的绝对路径。展开结果仍受部署期白名单约束。
    ///
    /// # 错误
    ///
    /// 项目目录不是绝对路径或挂载书写无效时返回错误。
    pub fn resolve_relative_mounts(&mut self, project_directory: &Path) -> anyhow::Result<()> {
        if !project_directory.is_absolute() {
            anyhow::bail!("项目目录必须是绝对路径: {}", project_directory.display());
        }
        for (name, service) in &mut self.document.services {
            for value in &mut service.volumes {
                let mount = BindMount::parse(value)?;
                if mount.is_project_relative() {
                    *value = BindMount {
                        // 归一化掉 `.` 分量，得到 `/…/<项目>/files/x.yaml` 这样的路径。
                        host_path: super::value::normalize_absolute_path(
                            &mount.resolved_host_path(project_directory),
                        )?
                        .display()
                        .to_string(),
                        ..mount
                    }
                    .compose_value();
                }
            }
            if service.volumes.iter().any(|value| value.is_empty()) {
                anyhow::bail!("服务 {name} 的 bind mount 展开为空");
            }
        }
        Ok(())
    }

    /// 返回仅由 IR 生成的规范化 Compose YAML。
    ///
    /// # 错误
    ///
    /// 序列化意外失败时返回错误。
    pub fn compose_yaml(&self) -> anyhow::Result<String> {
        let mut output = serde_yaml::to_string(&self.document)?;
        if !output.ends_with('\n') {
            output.push('\n');
        }
        Ok(output)
    }

    /// 返回确定顺序的 `.env` 表示。
    #[must_use]
    pub fn env_file(&self) -> String {
        serialize_env_file(&self.environment)
    }

    /// 返回全部已发布宿主机端口。
    ///
    /// # 错误
    ///
    /// 存储的映射格式错误时返回错误。
    pub fn host_ports(&self) -> anyhow::Result<Vec<PublishedPort>> {
        self.document
            .services
            .values()
            .flat_map(|service| service.ports.iter())
            .map(|value| PublishedPort::parse(value))
            .collect()
    }

    /// 返回全部由 nsetup 生成的路由绑定，用于跨项目冲突检查与诊断。
    ///
    /// # 错误
    ///
    /// 生成的路由 label 无效时返回错误。
    pub fn route_bindings(&self) -> anyhow::Result<Vec<RouteBinding>> {
        let mut bindings = Vec::new();
        for (service_name, service) in &self.document.services {
            for route in service.routes(&self.name, service_name)? {
                bindings.extend(route.bindings(&self.name, service_name));
            }
        }
        Ok(bindings)
    }

    /// 返回 Traefik label 声明的全部主机名。
    ///
    /// # 错误
    ///
    /// 路由 label 不一致时返回错误。
    pub fn route_hosts(&self) -> anyhow::Result<Vec<String>> {
        let mut hosts = Vec::new();
        for service in self.document.services.values() {
            for (key, value) in label_map(&service.labels)? {
                if key.starts_with("traefik.http.routers.")
                    && key.ends_with(".rule")
                    && value.contains("Host(")
                {
                    for host in parse_hosts(&value)? {
                        validate_domain(&host)?;
                        hosts.push(host);
                    }
                }
            }
        }
        Ok(hosts)
    }

    /// 返回在项目中声明的全部主域名。
    ///
    /// 用于 `config set domain` 变更前提示哪些项目显式钉定了旧域名。
    ///
    /// # 错误
    ///
    /// 路由 label 无效时返回错误。
    pub fn domains(&self) -> anyhow::Result<BTreeSet<String>> {
        let mut domains = BTreeSet::new();
        for host in self.route_hosts()? {
            if let Some(rest) = host.split_once('.') {
                domains.insert(rest.1.to_string());
            }
        }
        Ok(domains)
    }

    /// 返回全部服务启动钩子，按 Compose 服务名排序。
    ///
    /// # 错误
    ///
    /// 项目环境中的钩子记录不是合法 JSON 时返回错误。
    pub fn project_hooks(&self) -> anyhow::Result<BTreeMap<String, ServiceHooks>> {
        match self.environment.get(super::HOOKS_KEY) {
            Some(value) => Ok(serde_json::from_str(value)?),
            None => Ok(BTreeMap::new()),
        }
    }

    /// 记录服务启动钩子，仅保存非空条目。
    ///
    /// # 错误
    ///
    /// 序列化失败时返回错误。
    pub fn set_project_hooks(
        &mut self,
        hooks: &BTreeMap<String, ServiceHooks>,
    ) -> anyhow::Result<()> {
        let hooks: BTreeMap<_, _> = hooks
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if hooks.is_empty() {
            self.environment.remove(super::HOOKS_KEY);
            return Ok(());
        }
        self.environment.insert(
            String::from(super::HOOKS_KEY),
            serde_json::to_string(&hooks)?,
        );
        Ok(())
    }

    /// 返回指定服务在给定阶段的钩子命令。
    ///
    /// # 错误
    ///
    /// 项目环境中的钩子记录不是合法 JSON 时返回错误。
    pub fn service_hooks(&self, stage: super::HookStage) -> anyhow::Result<Vec<(String, String)>> {
        let hooks = self.project_hooks()?;
        let mut commands = Vec::new();
        for (service, hook) in hooks {
            for command in hook.commands(stage) {
                commands.push((service.clone(), command.clone()));
            }
        }
        Ok(commands)
    }
}

/// 使用项目环境状态解析镜像中的 `${KEY}` 片段。
fn interpolate_image(
    image: &str,
    environment: &BTreeMap<String, String>,
) -> anyhow::Result<String> {
    let mut result = String::new();
    let mut remaining = image;
    while let Some(start) = remaining.find("${") {
        result.push_str(&remaining[..start]);
        let after = &remaining[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("镜像变量缺少 }}: {image}"))?;
        let key = &after[..end];
        let value = environment
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("镜像变量 {key} 未在 .env 中定义"))?;
        result.push_str(value);
        remaining = &after[end + 1..];
    }
    result.push_str(remaining);
    Ok(result)
}

/// 解析 nsetup 管理的保守 `.env` 子集。
pub(super) fn parse_env_file(input: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for (index, original) in input.lines().enumerate() {
        let line = original.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, raw_value) = line
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!(".env 第 {} 行缺少 =", index + 1))?;
        validate_env_key(key)?;
        let value = unquote_env(raw_value)?;
        if output.insert(key.to_string(), value).is_some() {
            anyhow::bail!(".env 重复定义变量 {key}");
        }
    }
    Ok(output)
}

/// 以确定顺序序列化项目变量，并保护字面量 `$` 不被 Compose 插值。
pub(super) fn serialize_env_file(environment: &BTreeMap<String, String>) -> String {
    let mut output = String::new();
    for (key, value) in environment {
        if value.contains('$') && !value.contains(['\n', '\r']) {
            let escaped = value.replace('\'', "\\'");
            output.push_str(&format!("{key}='{escaped}'\n"));
        } else {
            let escaped = value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r");
            output.push_str(&format!("{key}=\"{escaped}\"\n"));
        }
    }
    output
}

/// 移除受支持的 `.env` 引号和转义序列。
fn unquote_env(value: &str) -> anyhow::Result<String> {
    if value.starts_with('"') {
        if !value.ends_with('"') || value.len() < 2 {
            anyhow::bail!(".env 双引号未闭合");
        }
        let mut output = String::new();
        let mut escaped = false;
        for character in value[1..value.len() - 1].chars() {
            if escaped {
                output.push(match character {
                    'n' => '\n',
                    'r' => '\r',
                    other => other,
                });
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else {
                output.push(character);
            }
        }
        if escaped {
            anyhow::bail!(".env 转义不完整");
        }
        Ok(output)
    } else if value.starts_with('\'') {
        if !value.ends_with('\'') || value.len() < 2 {
            anyhow::bail!(".env 单引号未闭合");
        }
        let mut output = String::new();
        let mut characters = value[1..value.len() - 1].chars().peekable();
        while let Some(character) = characters.next() {
            if character == '\\' && characters.peek() == Some(&'\'') {
                let _quote = characters.next();
                output.push('\'');
            } else {
                output.push(character);
            }
        }
        Ok(output)
    } else {
        Ok(value.trim().to_string())
    }
}

/// 校验可移植的环境变量键。
fn validate_env_key(key: &str) -> anyhow::Result<()> {
    let mut bytes = key.bytes();
    let first = bytes
        .next()
        .ok_or_else(|| anyhow::anyhow!("环境变量名不能为空"))?;
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        anyhow::bail!("环境变量名无效: {key}");
    }
    Ok(())
}
