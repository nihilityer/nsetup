//! 按参数局部修改已有 Compose 应用。

use super::{
    InvalidInput, ensure_regular_stack_dir, image_repository, stack_dir, update_stack,
    valid_image_version, validate_service_name,
};
use crate::config::Config;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::generator::{
    self, AppSpec, HealthcheckSpec, Middleware, NamedVolume, NetworkMode, PublishedPort, Route,
    Volume,
};
use anyhow::Context;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::fs;

/// 参数化修改已有应用的请求；未提供的字段保持原样。
#[derive(Debug, Clone)]
pub struct ApplicationEdit {
    /// Compose 项目名。
    pub name: String,
    /// 要修改的 Compose 服务名。
    pub service: Option<String>,
    /// 新的镜像仓库名。
    pub image: Option<String>,
    /// 新的镜像版本标签。
    pub version: Option<String>,
    /// 覆盖镜像默认命令的参数列表。
    pub command: Vec<String>,
    /// 路由默认容器端口。
    pub container_port: Option<u16>,
    /// 重建路由使用的访问域名。
    pub hosts: Vec<String>,
    /// 重建路由使用的显式路由。
    pub routes: Vec<Route>,
    /// 应用于全部路由的 URL 路径前缀。
    pub path_prefix: Option<String>,
    /// 替换后的宿主机端口映射。
    pub published_ports: Vec<PublishedPort>,
    /// 追加的卷挂载。
    pub volumes: Vec<Volume>,
    /// 合并的环境变量。
    pub environment: BTreeMap<String, String>,
    /// 新的网络模式。
    pub network_mode: Option<NetworkMode>,
    /// 替换的 Traefik 中间件。
    pub middlewares: Vec<Middleware>,
    /// 按标签键替换的自定义 Docker 标签。
    pub labels: Vec<String>,
    /// 追加的命名卷。
    pub named_volumes: Vec<NamedVolume>,
    /// 新的健康检查。
    pub healthcheck: Option<HealthcheckSpec>,
    /// 是否移除现有健康检查。
    pub remove_healthcheck: bool,
    /// 是否在修改成功后执行 `compose up`。
    pub start: bool,
}

/// 读取并解析现有 Compose 项目，应用参数修改后重新部署。
pub fn update_application(config: &Config, edit: &ApplicationEdit) -> anyhow::Result<()> {
    let directory = stack_dir(config, &edit.name)?;
    ensure_regular_stack_dir(&directory)?;
    let compose_path = directory.join(COMPOSE_FILE);
    let env_path = directory.join(ENV_FILE);
    let compose = fs::read_to_string(&compose_path)
        .with_context(|| format!("无法读取 Compose 文件: {}", compose_path.display()))?;
    let env_file = fs::read_to_string(&env_path)
        .with_context(|| format!("无法读取环境变量文件: {}", env_path.display()))?;
    let mut document: Value = serde_yaml::from_str(&compose)
        .map_err(|error| InvalidInput(format!("Compose YAML 格式错误: {error}")))?;
    apply_edit(&mut document, edit)?;
    let updated = serde_yaml::to_string(&document).context("无法序列化修改后的 Compose 配置")?;
    update_stack(config, &edit.name, &updated, Some(&env_file), edit.start)
}

/// 向已有 Compose 项目追加一个由参数生成的服务。
pub fn add_service(
    config: &Config,
    spec: &AppSpec,
    force: bool,
    start: bool,
) -> anyhow::Result<()> {
    let directory = stack_dir(config, &spec.name)?;
    ensure_regular_stack_dir(&directory)?;
    let compose_path = directory.join(COMPOSE_FILE);
    let env_path = directory.join(ENV_FILE);
    let compose = fs::read_to_string(&compose_path)
        .with_context(|| format!("无法读取 Compose 文件: {}", compose_path.display()))?;
    let env_file = fs::read_to_string(&env_path)
        .with_context(|| format!("无法读取环境变量文件: {}", env_path.display()))?;
    let mut document: Value = serde_yaml::from_str(&compose)
        .map_err(|error| InvalidInput(format!("Compose YAML 格式错误: {error}")))?;
    add_service_to_document(&mut document, spec, force)?;
    let updated = serde_yaml::to_string(&document).context("无法序列化修改后的 Compose 配置")?;
    update_stack(config, &spec.name, &updated, Some(&env_file), start)
}

/// 将参数生成的服务写入 Compose 文档（纯变换，便于单元测试）。
fn add_service_to_document(
    document: &mut Value,
    spec: &AppSpec,
    force: bool,
) -> anyhow::Result<()> {
    let fragment = {
        let services = document
            .as_mapping_mut()
            .and_then(|mapping| mapping.get_mut(Value::String(String::from("services"))))
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| InvalidInput(String::from("Compose YAML 必须包含 services 对象")))?;
        let exists = services.contains_key(Value::String(spec.service.clone()));
        if exists && !force {
            return Err(InvalidInput(format!(
                "服务 {} 已存在；确认替换请使用 --force",
                spec.service
            ))
            .into());
        }
        if exists {
            services.remove(Value::String(spec.service.clone()));
        }
        let route_offset = project_route_offset(services, &spec.name);
        let container_name = format!("nihility-{}-{}", spec.name, spec.service);
        let (service, fragment) = generator::build_service(spec, route_offset, &container_name)?;
        let mut service_value = serde_yaml::to_value(&service)
            .map_err(|error| InvalidInput(format!("无法序列化生成的服务: {error}")))?;
        if let Some(service_map) = service_value.as_mapping_mut()
            && !spec.environment.is_empty()
        {
            let environment = ensure_mapping(service_map, "environment")?;
            for (key, value) in &spec.environment {
                environment.insert(Value::String(key.clone()), Value::String(value.clone()));
            }
        }
        services.insert(Value::String(spec.service.clone()), service_value);
        fragment
    };
    let top = document
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("Compose 顶层必须是对象"))?;
    let network_map = ensure_mapping(top, "networks")?;
    for (name, network) in &fragment.networks {
        let value = serde_yaml::to_value(network)
            .map_err(|error| InvalidInput(format!("无法序列化网络定义: {error}")))?;
        network_map.insert(Value::String(name.clone()), value);
    }
    let volume_map = ensure_mapping(top, "volumes")?;
    for name in fragment.volumes.keys() {
        volume_map
            .entry(Value::String(name.clone()))
            .or_insert(Value::Mapping(Mapping::new()));
    }
    Ok(())
}

/// 将参数修改应用到 Compose 文档（纯变换，便于单元测试）。
fn apply_edit(document: &mut Value, edit: &ApplicationEdit) -> anyhow::Result<()> {
    let selected = {
        let services = document
            .as_mapping_mut()
            .and_then(|mapping| mapping.get_mut(Value::String(String::from("services"))))
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| InvalidInput(String::from("Compose YAML 必须包含 services 对象")))?;
        select_service(services, edit.service.as_deref())?
    };
    {
        let services = document
            .as_mapping_mut()
            .and_then(|mapping| mapping.get_mut(Value::String(String::from("services"))))
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| InvalidInput(String::from("Compose YAML 必须包含 services 对象")))?;
        let service = match services.get_mut(Value::String(selected.clone())) {
            Some(Value::Mapping(service)) => service,
            _ => return Err(InvalidInput(format!("Compose 服务不存在: {selected}")).into()),
        };
        apply_image(service, edit)?;
        apply_command(service, edit);
        apply_environment(service, edit)?;
        apply_ports(service, edit);
        apply_service_volumes(service, edit)?;
        apply_service_networks(service, edit)?;
        apply_routing(service, edit)?;
        apply_labels(service, edit)?;
        apply_healthcheck(service, edit)?;
    }
    apply_declared_volumes(document, edit)?;
    apply_declared_networks(document, edit)?;
    Ok(())
}

/// 选择要修改的 Compose 服务。
fn select_service(services: &Mapping, requested: Option<&str>) -> anyhow::Result<String> {
    match requested {
        Some(service) => {
            validate_service_name(service)?;
            if !services.contains_key(Value::String(service.to_string())) {
                return Err(InvalidInput(format!("Compose 服务不存在: {service}")).into());
            }
            Ok(service.to_string())
        }
        None if services.len() == 1 => services
            .keys()
            .next()
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| InvalidInput(String::from("Compose 服务名必须是字符串")).into()),
        None => Err(InvalidInput(String::from(
            "该应用包含多个服务，请使用 --service 指定要修改的 Compose 服务",
        ))
        .into()),
    }
}

/// 应用镜像与版本修改。
fn apply_image(service: &mut Mapping, edit: &ApplicationEdit) -> anyhow::Result<()> {
    if edit.image.is_none() && edit.version.is_none() {
        return Ok(());
    }
    let current = service
        .get(Value::String(String::from("image")))
        .and_then(Value::as_str)
        .ok_or_else(|| InvalidInput(String::from("Compose 服务没有声明 image，无法修改镜像")))?;
    let repository = match &edit.image {
        Some(image) => {
            validate_image_repository(image)?;
            image.clone()
        }
        None => image_repository(current)?.to_string(),
    };
    let image = match &edit.version {
        Some(version) => {
            if !valid_image_version(version) {
                return Err(InvalidInput(String::from(
                    "镜像版本不能使用 latest，且必须以字母、数字或下划线开头，只能包含字母、数字、点、下划线和连字符，且不超过 128 个字符",
                ))
                .into());
            }
            format!("{repository}:{version}")
        }
        None => format!("{repository}:{}", current_tag(current)?),
    };
    service.insert(Value::String(String::from("image")), Value::String(image));
    Ok(())
}

/// 校验镜像仓库名不含标签或摘要。
fn validate_image_repository(image: &str) -> anyhow::Result<()> {
    if image.trim().is_empty() || image.contains(['\n', '\r', '\0', '@']) {
        return Err(InvalidInput(String::from("镜像名不能为空、不能包含摘要或控制字符")).into());
    }
    if image_repository(image)? != image {
        return Err(InvalidInput(String::from(
            "镜像名不能包含版本标签；请使用 --version 指定标签",
        ))
        .into());
    }
    Ok(())
}

/// 提取现有镜像的版本标签。
fn current_tag(image: &str) -> anyhow::Result<String> {
    if image.contains('@') {
        return Err(InvalidInput(String::from("按镜像摘要部署的项目无法通过参数修改镜像")).into());
    }
    let repository = image_repository(image)?;
    let tag = image
        .strip_prefix(repository)
        .and_then(|rest| rest.strip_prefix(':'))
        .filter(|tag| !tag.is_empty());
    tag.map(str::to_string)
        .ok_or_else(|| InvalidInput(String::from("现有镜像缺少版本标签")).into())
}

/// 应用命令修改。
fn apply_command(service: &mut Mapping, edit: &ApplicationEdit) {
    if edit.command.is_empty() {
        return;
    }
    service.insert(
        Value::String(String::from("command")),
        Value::Sequence(edit.command.iter().cloned().map(Value::String).collect()),
    );
}

/// 合并环境变量到服务定义。
fn apply_environment(service: &mut Mapping, edit: &ApplicationEdit) -> anyhow::Result<()> {
    if edit.environment.is_empty() {
        return Ok(());
    }
    let environment = ensure_mapping(service, "environment")?;
    for (key, value) in &edit.environment {
        environment.insert(Value::String(key.clone()), Value::String(value.clone()));
    }
    Ok(())
}

/// 替换发布端口列表。
fn apply_ports(service: &mut Mapping, edit: &ApplicationEdit) {
    if edit.published_ports.is_empty() {
        return;
    }
    let ports = edit
        .published_ports
        .iter()
        .map(|port| {
            Value::String(format!(
                "{}:{}/{}",
                port.host_port,
                port.container_port,
                port.protocol.compose_suffix()
            ))
        })
        .collect();
    service.insert(Value::String(String::from("ports")), Value::Sequence(ports));
}

/// 追加卷挂载到服务定义。
fn apply_service_volumes(service: &mut Mapping, edit: &ApplicationEdit) -> anyhow::Result<()> {
    if !edit.volumes.is_empty() {
        let volumes = ensure_sequence(service, "volumes")?;
        for volume in &edit.volumes {
            let mount = format!(
                "{}:{}{}",
                volume.host_path,
                volume.container_path,
                if volume.read_only { ":ro" } else { "" }
            );
            append_unique(volumes, &mount);
        }
    }
    if !edit.named_volumes.is_empty() {
        let volumes = ensure_sequence(service, "volumes")?;
        for volume in &edit.named_volumes {
            append_unique(
                volumes,
                &format!("{}:{}", volume.name, volume.container_path),
            );
        }
    }
    Ok(())
}

/// 在 Compose 顶层声明追加的命名卷。
fn apply_declared_volumes(document: &mut Value, edit: &ApplicationEdit) -> anyhow::Result<()> {
    if edit.named_volumes.is_empty() {
        return Ok(());
    }
    let top = document
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("Compose 顶层必须是对象"))?;
    let declared = ensure_mapping(top, "volumes")?;
    for volume in &edit.named_volumes {
        declared
            .entry(Value::String(volume.name.clone()))
            .or_insert(Value::Mapping(Mapping::new()));
    }
    Ok(())
}

/// 应用服务网络模式修改。
fn apply_service_networks(service: &mut Mapping, edit: &ApplicationEdit) -> anyhow::Result<()> {
    let Some(network_mode) = &edit.network_mode else {
        return Ok(());
    };
    match network_mode {
        NetworkMode::Bridge => {
            service.remove(Value::String(String::from("network_mode")));
            remove_network_ref(service, "application")?;
        }
        NetworkMode::Host => {
            service.insert(
                Value::String(String::from("network_mode")),
                Value::String(String::from("host")),
            );
            remove_network_ref(service, "application")?;
            remove_network_ref(service, "traefik")?;
        }
        NetworkMode::External(_) => {
            service.remove(Value::String(String::from("network_mode")));
            let networks = ensure_sequence(service, "networks")?;
            append_unique(networks, "application");
        }
    }
    Ok(())
}

/// 在 Compose 顶层声明 external 网络。
fn apply_declared_networks(document: &mut Value, edit: &ApplicationEdit) -> anyhow::Result<()> {
    let Some(NetworkMode::External(name)) = &edit.network_mode else {
        return Ok(());
    };
    let top = document
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("Compose 顶层必须是对象"))?;
    let network_map = ensure_mapping(top, "networks")?;
    let mut definition = Mapping::new();
    definition.insert(
        Value::String(String::from("name")),
        Value::String(name.clone()),
    );
    definition.insert(Value::String(String::from("external")), Value::Bool(true));
    network_map.insert(
        Value::String(String::from("application")),
        Value::Mapping(definition),
    );
    Ok(())
}

/// 应用路由、端口、中间件与标签相关修改。
fn apply_routing(service: &mut Mapping, edit: &ApplicationEdit) -> anyhow::Result<()> {
    let rebuild = !edit.hosts.is_empty() || !edit.routes.is_empty() || edit.path_prefix.is_some();
    if !rebuild {
        if let Some(port) = edit.container_port {
            update_loadbalancer_ports(service, &edit.name, port);
        }
        if !edit.middlewares.is_empty() {
            update_middleware_labels(service, &edit.name, &edit.middlewares);
        }
        return Ok(());
    }

    let default_port = edit
        .container_port
        .unwrap_or_else(|| existing_container_port(service, &edit.name).unwrap_or(80));
    let mut routes = Vec::new();
    for host in &edit.hosts {
        routes.push(Route {
            host: host.clone(),
            path_prefix: edit.path_prefix.clone(),
            container_port: default_port,
        });
    }
    for route in &edit.routes {
        routes.push(Route {
            host: route.host.clone(),
            path_prefix: edit
                .path_prefix
                .clone()
                .or_else(|| route.path_prefix.clone()),
            container_port: route.container_port,
        });
    }
    if routes.is_empty() {
        // 仅提供 path_prefix 时保留并更新现有路由。
        let mut existing = parse_existing_routes(service, &edit.name)?;
        for route in &mut existing {
            if let Some(prefix) = &edit.path_prefix {
                route.path_prefix = Some(prefix.clone());
            }
        }
        routes = existing;
    }
    let middlewares = if edit.middlewares.is_empty() {
        parse_existing_middlewares(service, &edit.name)
    } else {
        edit.middlewares.clone()
    };
    let route_offset = existing_route_offset(service, &edit.name);
    remove_generated_route_labels(service, &edit.name);
    let labels =
        generator::application_route_labels(&edit.name, &routes, &middlewares, route_offset);
    let list = ensure_sequence(service, "labels")?;
    for label in labels {
        if !list
            .iter()
            .any(|value| value.as_str() == Some(label.as_str()))
        {
            list.push(Value::String(label));
        }
    }
    Ok(())
}

/// 按标签键替换自定义标签。
fn apply_labels(service: &mut Mapping, edit: &ApplicationEdit) -> anyhow::Result<()> {
    if edit.labels.is_empty() {
        return Ok(());
    }
    let labels = ensure_sequence(service, "labels")?;
    for label in &edit.labels {
        let Some((key, _)) = label.split_once('=') else {
            return Err(InvalidInput(String::from("自定义标签格式必须为 KEY=VALUE")).into());
        };
        let prefix = format!("{key}=");
        labels.retain(|value| {
            value
                .as_str()
                .is_none_or(|existing| !existing.starts_with(&prefix))
        });
        labels.push(Value::String(label.clone()));
    }
    Ok(())
}

/// 应用健康检查修改。
fn apply_healthcheck(service: &mut Mapping, edit: &ApplicationEdit) -> anyhow::Result<()> {
    if edit.remove_healthcheck {
        service.remove(Value::String(String::from("healthcheck")));
    }
    let Some(healthcheck) = &edit.healthcheck else {
        return Ok(());
    };
    validate_duration(&healthcheck.interval, "健康检查间隔")?;
    validate_duration(&healthcheck.timeout, "健康检查超时")?;
    if let Some(start_period) = &healthcheck.start_period {
        validate_duration(start_period, "健康检查启动宽限期")?;
    }
    if healthcheck.command.trim().is_empty() || healthcheck.command.contains(['\0', '\n', '\r']) {
        return Err(InvalidInput(String::from("健康检查命令不能为空或包含控制字符")).into());
    }
    let mut definition = Mapping::new();
    definition.insert(
        Value::String(String::from("test")),
        Value::Sequence(vec![
            Value::String(String::from("CMD-SHELL")),
            Value::String(healthcheck.command.clone()),
        ]),
    );
    definition.insert(
        Value::String(String::from("interval")),
        Value::String(healthcheck.interval.clone()),
    );
    definition.insert(
        Value::String(String::from("timeout")),
        Value::String(healthcheck.timeout.clone()),
    );
    if let Some(start_period) = &healthcheck.start_period {
        definition.insert(
            Value::String(String::from("start_period")),
            Value::String(start_period.clone()),
        );
    }
    definition.insert(
        Value::String(String::from("retries")),
        Value::Number(serde_yaml::Number::from(u64::from(healthcheck.retries))),
    );
    service.insert(
        Value::String(String::from("healthcheck")),
        Value::Mapping(definition),
    );
    Ok(())
}

/// 校验时长参数格式。
fn validate_duration(value: &str, label: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() || value.contains(['\0', '\n', '\r', ' ']) {
        return Err(InvalidInput(format!("{label}格式无效: {value}")).into());
    }
    Ok(())
}

/// 解析现有生成的默认容器端口。
fn existing_container_port(service: &Mapping, name: &str) -> Option<u16> {
    let labels = service
        .get(Value::String(String::from("labels")))?
        .as_sequence()?;
    let prefix = format!("traefik.http.services.{name}-");
    labels.iter().find_map(|label| {
        let text = label.as_str()?;
        let (key, value) = text.split_once('=')?;
        if is_generated_key(key, &prefix) && key.ends_with(".loadbalancer.server.port") {
            value.parse().ok()
        } else {
            None
        }
    })
}

/// 解析现有生成路由的域名、前缀与端口。
fn parse_existing_routes(service: &Mapping, name: &str) -> anyhow::Result<Vec<Route>> {
    let mut routes = Vec::new();
    let Some(labels) = service
        .get(Value::String(String::from("labels")))
        .and_then(Value::as_sequence)
    else {
        return Ok(routes);
    };
    let router_prefix = format!("traefik.http.routers.{name}-");
    for label in labels {
        let Some(text) = label.as_str() else {
            continue;
        };
        let Some((key, rule)) = text.split_once('=') else {
            continue;
        };
        if !is_generated_key(key, &router_prefix) || !key.ends_with(".rule") {
            continue;
        }
        let Some(router) = key
            .strip_prefix("traefik.http.routers.")
            .and_then(|rest| rest.strip_suffix(".rule"))
        else {
            continue;
        };
        let Some(host) = extract_host(rule) else {
            continue;
        };
        routes.push(Route {
            host,
            path_prefix: extract_prefix(rule),
            container_port: router_port(service, router),
        });
    }
    Ok(routes)
}

/// 从规则中提取 `Host(...)` 域名。
fn extract_host(rule: &str) -> Option<String> {
    let rest = rule.strip_prefix("Host(`")?;
    let end = rest.find("`)")?;
    Some(rest[..end].to_string())
}

/// 从规则中提取 `PathPrefix(...)` 前缀。
fn extract_prefix(rule: &str) -> Option<String> {
    let rest = rule.split_once("PathPrefix(`")?.1;
    let end = rest.find("`)")?;
    Some(rest[..end].to_string())
}

/// 查询路由对应的负载均衡端口。
fn router_port(service: &Mapping, router: &str) -> u16 {
    let key = format!("traefik.http.services.{router}.loadbalancer.server.port=");
    service
        .get(Value::String(String::from("labels")))
        .and_then(Value::as_sequence)
        .and_then(|labels| {
            labels.iter().find_map(|label| {
                label
                    .as_str()
                    .and_then(|text| text.strip_prefix(&key))
                    .and_then(|port| port.parse().ok())
            })
        })
        .unwrap_or(80)
}

/// 计算服务现有生成路由的最小序号；重建路由时沿用该起点避免跨服务重名。
fn existing_route_offset(service: &Mapping, name: &str) -> usize {
    let Some(labels) = service
        .get(Value::String(String::from("labels")))
        .and_then(Value::as_sequence)
    else {
        return 0;
    };
    let prefix = format!("traefik.http.routers.{name}-");
    labels
        .iter()
        .filter_map(|label| {
            let text = label.as_str()?;
            let (key, _) = text.split_once('=')?;
            if !is_generated_key(key, &prefix) || !key.ends_with(".rule") {
                return None;
            }
            key.strip_prefix(&prefix)?
                .split('.')
                .next()?
                .parse::<usize>()
                .ok()
        })
        .min()
        .unwrap_or(0)
}

/// 统计项目内已使用的路由序号，返回下一个可用起点。
fn project_route_offset(services: &Mapping, project: &str) -> usize {
    let prefix = format!("traefik.http.routers.{project}-");
    let mut max_index: Option<usize> = None;
    for service in services.values() {
        let Some(labels) = service
            .get(Value::String(String::from("labels")))
            .and_then(Value::as_sequence)
        else {
            continue;
        };
        for label in labels {
            let Some(text) = label.as_str() else {
                continue;
            };
            let Some((key, _)) = text.split_once('=') else {
                continue;
            };
            if !is_generated_key(key, &prefix) || !key.ends_with(".rule") {
                continue;
            }
            if let Some(index) = key
                .strip_prefix(&prefix)
                .and_then(|rest| rest.split('.').next())
                .and_then(|digits| digits.parse::<usize>().ok())
            {
                max_index = Some(max_index.map_or(index, |current| current.max(index)));
            }
        }
    }
    max_index.map_or(0, |index| index + 1)
}

/// 解析现有生成路由的中间件引用。
fn parse_existing_middlewares(service: &Mapping, name: &str) -> Vec<Middleware> {
    let mut middlewares = Vec::new();
    let Some(labels) = service
        .get(Value::String(String::from("labels")))
        .and_then(Value::as_sequence)
    else {
        return middlewares;
    };
    let router_prefix = format!("traefik.http.routers.{name}-");
    for label in labels {
        let Some(text) = label.as_str() else {
            continue;
        };
        let Some((key, value)) = text.split_once('=') else {
            continue;
        };
        if !is_generated_key(key, &router_prefix) || !key.ends_with(".middlewares") {
            continue;
        }
        for reference in value.split(',') {
            if let Some(middleware) = Middleware::from_label_ref(reference)
                && !middlewares.contains(&middleware)
            {
                middlewares.push(middleware);
            }
        }
    }
    middlewares
}

/// 移除生成的路由与负载均衡标签，保留自定义标签。
fn remove_generated_route_labels(service: &mut Mapping, name: &str) {
    if let Some(Value::Sequence(labels)) = service.get_mut(Value::String(String::from("labels"))) {
        let router_prefix = format!("traefik.http.routers.{name}-");
        let service_prefix = format!("traefik.http.services.{name}-");
        labels.retain(|value| {
            value.as_str().is_none_or(|label| {
                let key = label.split_once('=').map_or(label, |(key, _)| key);
                !is_generated_key(key, &router_prefix) && !is_generated_key(key, &service_prefix)
            })
        });
    }
}

/// 更新生成路由的负载均衡端口标签。
fn update_loadbalancer_ports(service: &mut Mapping, name: &str, port: u16) {
    if let Some(Value::Sequence(labels)) = service.get_mut(Value::String(String::from("labels"))) {
        let prefix = format!("traefik.http.services.{name}-");
        for label in labels.iter_mut() {
            let Some(text) = label.as_str() else {
                continue;
            };
            let Some((key, _)) = text.split_once('=') else {
                continue;
            };
            if is_generated_key(key, &prefix) && key.ends_with(".loadbalancer.server.port") {
                *label = Value::String(format!("{key}={port}"));
            }
        }
    }
}

/// 更新生成路由的中间件标签。
fn update_middleware_labels(service: &mut Mapping, name: &str, middlewares: &[Middleware]) {
    if let Some(Value::Sequence(labels)) = service.get_mut(Value::String(String::from("labels"))) {
        let prefix = format!("traefik.http.routers.{name}-");
        let joined = middlewares
            .iter()
            .map(|middleware| middleware.label_ref())
            .collect::<Vec<_>>()
            .join(",");
        for label in labels.iter_mut() {
            let Some(text) = label.as_str() else {
                continue;
            };
            let Some((key, _)) = text.split_once('=') else {
                continue;
            };
            if is_generated_key(key, &prefix) && key.ends_with(".middlewares") {
                *label = Value::String(format!("{key}={joined}"));
            }
        }
    }
}

/// 判断标签键是否属于生成的路由或负载均衡标签。
fn is_generated_key(key: &str, prefix: &str) -> bool {
    let Some(rest) = key.strip_prefix(prefix) else {
        return false;
    };
    let digits = rest.split('.').next().unwrap_or_default();
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// 从服务网络列表中移除指定别名。
fn remove_network_ref(service: &mut Mapping, alias: &str) -> anyhow::Result<()> {
    match service.get_mut(Value::String(String::from("networks"))) {
        Some(Value::Sequence(networks)) => {
            networks.retain(|value| value.as_str() != Some(alias));
            Ok(())
        }
        Some(_) => Err(InvalidInput(String::from(
            "networks 必须是列表形式，暂不支持通过参数修改",
        ))
        .into()),
        None => Ok(()),
    }
}

/// 确保映射中存在指定键并返回其映射值。
fn ensure_mapping<'a>(mapping: &'a mut Mapping, key: &str) -> anyhow::Result<&'a mut Mapping> {
    if !mapping.contains_key(Value::String(key.to_string())) {
        mapping.insert(
            Value::String(key.to_string()),
            Value::Mapping(Mapping::new()),
        );
    }
    mapping
        .get_mut(Value::String(key.to_string()))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| InvalidInput(format!("{key} 必须是键值映射")).into())
}

/// 确保映射中存在指定键并返回其列表值。
fn ensure_sequence<'a>(mapping: &'a mut Mapping, key: &str) -> anyhow::Result<&'a mut Vec<Value>> {
    if !mapping.contains_key(Value::String(key.to_string())) {
        mapping.insert(Value::String(key.to_string()), Value::Sequence(Vec::new()));
    }
    mapping
        .get_mut(Value::String(key.to_string()))
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| InvalidInput(format!("{key} 必须是列表")).into())
}

/// 向列表追加不存在的字符串值。
fn append_unique(list: &mut Vec<Value>, value: &str) {
    if !list.iter().any(|existing| existing.as_str() == Some(value)) {
        list.push(Value::String(value.to_string()));
    }
}

#[cfg(test)]
// 测试断言直接暴露失败原因，允许在测试中调用 unwrap。
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::generator::{AppSpec, NamedVolume, NetworkMode, PortProtocol, PublishedPort, Route};

    /// 构造一个用于追加服务的参数定义。
    fn service_spec(name: &str, service: &str) -> AppSpec {
        AppSpec {
            name: name.to_string(),
            service: service.to_string(),
            image: String::from("example/worker"),
            version: String::from("1.0"),
            command: Vec::new(),
            container_port: 8081,
            routes: Vec::new(),
            published_ports: Vec::new(),
            volumes: Vec::new(),
            environment: BTreeMap::new(),
            network_mode: NetworkMode::Bridge,
            middlewares: Vec::new(),
            labels: Vec::new(),
            named_volumes: Vec::new(),
            healthcheck: None,
        }
    }

    /// 构造一个生成器风格的 Compose 文档。
    fn sample_document() -> Value {
        serde_yaml::from_str(
            r#"
services:
  app:
    image: traefik/whoami:v1.11
    container_name: nihility-whoami
    restart: unless-stopped
    ports:
      - "12780:8080/tcp"
    volumes:
      - /var/lib/whoami:/data
    env_file:
      - .env
    labels:
      - traefik.enable=true
      - traefik.docker.network=nihility-traefik
      - traefik.http.routers.whoami-0.rule=Host(`whoami.example.com`)
      - traefik.http.routers.whoami-0.entrypoints=https
      - traefik.http.routers.whoami-0.tls=true
      - traefik.http.routers.whoami-0.tls.certresolver=cloudflare
      - traefik.http.routers.whoami-0.service=whoami-0
      - traefik.http.services.whoami-0.loadbalancer.server.port=8080
      - traefik.http.routers.whoami-grpc.rule=Host(`whoami.example.com`) && (PathPrefix(`/grpc`))
    networks:
      - traefik
networks:
  traefik:
    name: nihility-traefik
    external: true
"#,
        )
        .unwrap()
    }

    /// 构造一个编辑请求。
    fn edit(name: &str) -> ApplicationEdit {
        ApplicationEdit {
            name: name.to_string(),
            service: None,
            image: None,
            version: None,
            command: Vec::new(),
            container_port: None,
            hosts: Vec::new(),
            routes: Vec::new(),
            path_prefix: None,
            published_ports: Vec::new(),
            volumes: Vec::new(),
            environment: BTreeMap::new(),
            network_mode: None,
            middlewares: Vec::new(),
            labels: Vec::new(),
            named_volumes: Vec::new(),
            healthcheck: None,
            remove_healthcheck: false,
            start: false,
        }
    }

    /// 验证版本、环境变量、端口与健康检查的局部修改。
    #[test]
    fn applies_scalar_and_healthcheck_edits() {
        let mut document = sample_document();
        let mut request = edit("whoami");
        request.version = Some(String::from("v1.12"));
        request
            .environment
            .insert(String::from("LOG_LEVEL"), String::from("info"));
        request.published_ports = vec![PublishedPort {
            host_port: 12781,
            container_port: 8081,
            protocol: PortProtocol::Tcp,
        }];
        request.healthcheck = Some(HealthcheckSpec {
            command: String::from("curl -f http://localhost:8080/health || exit 1"),
            interval: String::from("30s"),
            timeout: String::from("3s"),
            start_period: Some(String::from("10s")),
            retries: 5,
        });

        apply_edit(&mut document, &request).unwrap();
        let updated: Value =
            serde_yaml::from_str(&serde_yaml::to_string(&document).unwrap()).unwrap();
        let service = &updated["services"]["app"];
        assert_eq!(
            service["image"],
            Value::String(String::from("traefik/whoami:v1.12"))
        );
        assert_eq!(
            service["ports"],
            Value::Sequence(vec![Value::String(String::from("12781:8081/tcp"))])
        );
        assert_eq!(
            service["environment"]["LOG_LEVEL"],
            Value::String(String::from("info"))
        );
        let healthcheck = &service["healthcheck"];
        assert_eq!(
            healthcheck["test"],
            Value::Sequence(vec![
                Value::String(String::from("CMD-SHELL")),
                Value::String(String::from(
                    "curl -f http://localhost:8080/health || exit 1"
                )),
            ])
        );
        assert_eq!(healthcheck["interval"], Value::String(String::from("30s")));
        assert_eq!(
            healthcheck["retries"],
            Value::Number(serde_yaml::Number::from(5_u64))
        );
        assert_eq!(
            healthcheck["start_period"],
            Value::String(String::from("10s"))
        );
    }

    /// 验证 hosts 重建路由时保留自定义标签并替换生成标签。
    #[test]
    fn rebuilds_route_labels_and_keeps_custom_labels() {
        let mut document = sample_document();
        let mut request = edit("whoami");
        request.hosts = vec![String::from("new.example.com")];
        request.container_port = Some(9000);

        apply_edit(&mut document, &request).unwrap();
        let updated: Value =
            serde_yaml::from_str(&serde_yaml::to_string(&document).unwrap()).unwrap();
        let labels = updated["services"]["app"]["labels"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(labels.iter().any(|label| {
            *label == "traefik.http.routers.whoami-0.rule=Host(`new.example.com`)"
        }));
        assert!(labels.iter().any(|label| {
            *label == "traefik.http.services.whoami-0.loadbalancer.server.port=9000"
        }));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("whoami-grpc.rule"))
        );
    }

    /// 验证仅修改容器端口时更新负载均衡端口标签。
    #[test]
    fn updates_loadbalancer_port_only() {
        let mut document = sample_document();
        let mut request = edit("whoami");
        request.container_port = Some(9090);

        apply_edit(&mut document, &request).unwrap();
        let updated: Value =
            serde_yaml::from_str(&serde_yaml::to_string(&document).unwrap()).unwrap();
        let labels = updated["services"]["app"]["labels"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(labels.iter().any(|label| {
            *label == "traefik.http.services.whoami-0.loadbalancer.server.port=9090"
        }));
        assert_eq!(
            updated["services"]["app"]["image"],
            Value::String(String::from("traefik/whoami:v1.11"))
        );
    }

    /// 验证移除健康检查。
    #[test]
    fn removes_healthcheck() {
        let mut document = sample_document();
        document["services"]["app"]["healthcheck"] = Value::Mapping(Mapping::new());
        let mut request = edit("whoami");
        request.remove_healthcheck = true;

        apply_edit(&mut document, &request).unwrap();
        let updated: Value =
            serde_yaml::from_str(&serde_yaml::to_string(&document).unwrap()).unwrap();
        assert!(updated["services"]["app"].get("healthcheck").is_none());
    }

    /// 验证多服务项目必须指定服务名。
    #[test]
    fn multi_service_requires_service_name() {
        let document: Value = serde_yaml::from_str(
            r#"
services:
  web:
    image: example/web:1.0
  worker:
    image: example/worker:1.0
"#,
        )
        .unwrap();
        let mut document = document;
        let request = edit("multi");
        let result = apply_edit(&mut document, &request);
        assert!(result.is_err());
    }

    /// 验证向已有项目追加服务时使用下一个路由序号并合并网络与卷声明。
    #[test]
    fn appends_service_with_next_route_offset() {
        let mut document = sample_document();
        let mut spec = service_spec("whoami", "worker");
        spec.routes = vec![Route {
            host: String::from("worker.example.com"),
            path_prefix: None,
            container_port: 8081,
        }];
        spec.published_ports = vec![PublishedPort {
            host_port: 18081,
            container_port: 8081,
            protocol: PortProtocol::Tcp,
        }];
        spec.environment
            .insert(String::from("ROLE"), String::from("worker"));
        spec.network_mode = NetworkMode::External(String::from("extra-net"));
        spec.named_volumes = vec![NamedVolume {
            name: String::from("worker-data"),
            container_path: String::from("/var/lib/worker"),
        }];
        spec.labels = vec![String::from(
            "traefik.http.routers.whoami-worker.priority=1",
        )];

        add_service_to_document(&mut document, &spec, false).unwrap();
        let updated: Value =
            serde_yaml::from_str(&serde_yaml::to_string(&document).unwrap()).unwrap();
        let worker = &updated["services"]["worker"];
        assert_eq!(
            worker["container_name"],
            Value::String(String::from("nihility-whoami-worker"))
        );
        assert_eq!(
            worker["environment"]["ROLE"],
            Value::String(String::from("worker"))
        );
        let labels = worker["labels"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(labels.iter().any(|label| {
            *label == "traefik.http.routers.whoami-1.rule=Host(`worker.example.com`)"
        }));
        assert!(labels.contains(&"traefik.http.routers.whoami-worker.priority=1"));
        assert!(updated["volumes"]["worker-data"].is_mapping());
        assert_eq!(
            updated["networks"]["application"]["name"],
            Value::String(String::from("extra-net"))
        );
    }

    /// 验证重复服务名默认拒绝，--force 时替换。
    #[test]
    fn duplicate_service_rejected_unless_forced() {
        let mut document = sample_document();
        let spec = service_spec("whoami", "app");
        assert!(add_service_to_document(&mut document, &spec, false).is_err());

        add_service_to_document(&mut document, &spec, true).unwrap();
        let updated: Value =
            serde_yaml::from_str(&serde_yaml::to_string(&document).unwrap()).unwrap();
        assert_eq!(
            updated["services"]["app"]["image"],
            Value::String(String::from("example/worker:1.0"))
        );
        assert_eq!(
            updated["services"]["app"]["container_name"],
            Value::String(String::from("nihility-whoami-app"))
        );
    }
}
