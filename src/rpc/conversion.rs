//! gRPC 类型与内部生成模型的转换。

use super::proto::{
    ApplicationMiddleware, ApplicationNetworkMode, ApplicationVolume, Container, ContainerHealth,
    ContainerState, CreateApplicationRequest, CreateStaticSiteRequest, EnvironmentVariable,
    HealthcheckSpec as ProtoHealthcheckSpec, InitializeInfrastructureRequest,
    NamedVolume as ProtoNamedVolume, OperationResponse, PortProtocol, Stack,
    UpdateApplicationRequest,
};
use crate::generator::{
    self, AppSpec, HealthcheckSpec, InfraSpec, Middleware, NamedVolume, NetworkMode, PublishedPort,
    Route, StaticAsset, StaticSiteSpec, Volume,
};
use crate::orchestrator::{
    self, ApplicationEdit, ContainerHealth as InternalContainerHealth,
    ContainerState as InternalContainerState, StackInfo,
};
use std::collections::BTreeMap;
use tonic::{Response, Status};

/// 将常规应用协议请求转换为内部生成参数。
pub(super) fn application_spec(
    input: CreateApplicationRequest,
    configured_domain: &str,
) -> anyhow::Result<AppSpec> {
    let network_mode = match ApplicationNetworkMode::try_from(input.network_mode)
        .unwrap_or(ApplicationNetworkMode::Unspecified)
    {
        ApplicationNetworkMode::Unspecified | ApplicationNetworkMode::Bridge => NetworkMode::Bridge,
        ApplicationNetworkMode::Host => NetworkMode::Host,
        ApplicationNetworkMode::External => {
            if input.external_network.is_empty() {
                return Err(orchestrator::InvalidInput(String::from(
                    "external 网络模式必须指定网络名",
                ))
                .into());
            }
            NetworkMode::External(input.external_network)
        }
    };
    Ok(AppSpec {
        name: input.name,
        service: input.service,
        image: input.image,
        version: input.version,
        command: input.command,
        container_port: checked_port(input.container_port, "容器端口")?,
        routes: input
            .routes
            .into_iter()
            .map(|route| {
                Ok(Route {
                    host: resolve_host(&route.host, configured_domain)?,
                    path_prefix: (!route.path_prefix.is_empty()).then_some(route.path_prefix),
                    container_port: if route.container_port == 0 {
                        checked_port(input.container_port, "容器端口")?
                    } else {
                        checked_port(route.container_port, "路由容器端口")?
                    },
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        published_ports: published_ports(input.published_ports)?,
        volumes: application_volumes(input.volumes)?,
        environment: environment_map(input.environment)?,
        network_mode,
        middlewares: input
            .middlewares
            .into_iter()
            .map(middleware_from_proto)
            .collect::<anyhow::Result<Vec<_>>>()?,
        labels: custom_labels(input.labels)?,
        named_volumes: named_volumes(input.named_volumes)?,
        healthcheck: input.healthcheck.map(normalize_healthcheck).transpose()?,
    })
}

/// 将协议编辑请求转换为内部参数化编辑模型。
pub(super) fn application_edit(
    input: UpdateApplicationRequest,
    configured_domain: &str,
) -> anyhow::Result<ApplicationEdit> {
    let network_mode = input
        .network_mode
        .map(|value| {
            ApplicationNetworkMode::try_from(value).unwrap_or(ApplicationNetworkMode::Unspecified)
        })
        .map(|mode| -> anyhow::Result<NetworkMode> {
            match mode {
                ApplicationNetworkMode::Unspecified | ApplicationNetworkMode::Bridge => {
                    Ok(NetworkMode::Bridge)
                }
                ApplicationNetworkMode::Host => Ok(NetworkMode::Host),
                ApplicationNetworkMode::External => {
                    let name = input
                        .external_network
                        .clone()
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            orchestrator::InvalidInput(String::from(
                                "external 网络模式必须指定网络名",
                            ))
                        })?;
                    Ok(NetworkMode::External(name))
                }
            }
        })
        .transpose()?;
    if input.external_network.is_some() && !matches!(network_mode, Some(NetworkMode::External(_))) {
        return Err(orchestrator::InvalidInput(String::from(
            "--external-network 只能与 --network external 一起使用",
        ))
        .into());
    }
    let container_port = input
        .container_port
        .map(|port| checked_port(port, "容器端口"))
        .transpose()?;
    if let Some(version) = &input.version
        && !crate::orchestrator::valid_image_version(version)
    {
        return Err(orchestrator::InvalidInput(String::from(
            "镜像版本不能使用 latest，且必须以字母、数字或下划线开头，只能包含字母、数字、点、下划线和连字符，且不超过 128 个字符",
        ))
        .into());
    }
    if let Some(prefix) = &input.path_prefix
        && (!prefix.starts_with('/') || prefix.contains(['`', '\n', '\r']))
    {
        return Err(orchestrator::InvalidInput(String::from(
            "路由路径必须以 / 开头且不能包含控制字符",
        ))
        .into());
    }
    let mut routes = Vec::with_capacity(input.routes.len());
    for route in input.routes {
        routes.push(Route {
            host: resolve_host(&route.host, configured_domain)?,
            path_prefix: (!route.path_prefix.is_empty()).then_some(route.path_prefix),
            container_port: if route.container_port == 0 {
                container_port.ok_or_else(|| {
                    orchestrator::InvalidInput(String::from("配置 HTTP 路由时必须指定容器端口"))
                })?
            } else {
                checked_port(route.container_port, "路由容器端口")?
            },
        });
    }
    Ok(ApplicationEdit {
        name: input.name,
        service: input.service.filter(|service| !service.is_empty()),
        image: input.image.filter(|image| !image.is_empty()),
        version: input.version.filter(|version| !version.is_empty()),
        command: input.command,
        container_port,
        hosts: input
            .hosts
            .into_iter()
            .map(|host| resolve_host(&host, configured_domain))
            .collect::<anyhow::Result<Vec<_>>>()?,
        routes,
        path_prefix: input.path_prefix.filter(|prefix| !prefix.is_empty()),
        published_ports: published_ports(input.published_ports)?,
        volumes: application_volumes(input.volumes)?,
        environment: environment_map(input.environment)?,
        network_mode,
        middlewares: input
            .middlewares
            .into_iter()
            .map(middleware_from_proto)
            .collect::<anyhow::Result<Vec<_>>>()?,
        labels: custom_labels(input.labels)?,
        named_volumes: named_volumes(input.named_volumes)?,
        healthcheck: input.healthcheck.map(normalize_healthcheck).transpose()?,
        remove_healthcheck: input.remove_healthcheck,
        start: input.start,
    })
}

/// 收集并校验环境变量，拒绝重复键。
fn environment_map(input: Vec<EnvironmentVariable>) -> anyhow::Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for variable in input {
        if environment
            .insert(variable.key.clone(), variable.value)
            .is_some()
        {
            return Err(
                orchestrator::InvalidInput(format!("环境变量重复: {}", variable.key)).into(),
            );
        }
    }
    Ok(environment)
}

/// 校验并转换自定义标签，拒绝重复键。
fn custom_labels(input: Vec<String>) -> anyhow::Result<Vec<String>> {
    let mut labels = Vec::with_capacity(input.len());
    for label in input {
        let (key, value) = label.split_once('=').ok_or_else(|| {
            orchestrator::InvalidInput(String::from("自定义标签格式必须为 KEY=VALUE"))
        })?;
        if key.trim().is_empty()
            || key.contains(['\0', '\n', '\r'])
            || value.contains(['\0', '\n', '\r'])
        {
            return Err(
                orchestrator::InvalidInput(format!("自定义标签 {key} 的名称或值无效")).into(),
            );
        }
        if labels.iter().any(|existing: &String| {
            existing
                .split_once('=')
                .is_some_and(|(existing_key, _)| existing_key == key)
        }) {
            return Err(orchestrator::InvalidInput(format!("自定义标签重复: {key}")).into());
        }
        labels.push(label);
    }
    Ok(labels)
}

/// 转换并校验命名卷。
fn named_volumes(input: Vec<ProtoNamedVolume>) -> anyhow::Result<Vec<NamedVolume>> {
    input
        .into_iter()
        .map(|volume| {
            let valid_name = !volume.name.is_empty()
                && volume.name.len() <= 63
                && volume.name.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_' | b'.')
                });
            if !valid_name
                || !volume.container_path.starts_with('/')
                || volume.name.contains(['\0', '\n', '\r'])
                || volume.container_path.contains(['\0', '\n', '\r'])
            {
                return Err(
                    orchestrator::InvalidInput(String::from("命名卷名称或容器路径无效")).into(),
                );
            }
            Ok(NamedVolume {
                name: volume.name,
                container_path: volume.container_path,
            })
        })
        .collect()
}

/// 转换并校验卷挂载。
fn application_volumes(input: Vec<ApplicationVolume>) -> anyhow::Result<Vec<Volume>> {
    input
        .into_iter()
        .map(|volume| {
            if volume.host_path.is_empty()
                || !volume.container_path.starts_with('/')
                || volume.host_path.contains(['\0', '\n', '\r'])
                || volume.container_path.contains(['\0', '\n', '\r'])
            {
                return Err(orchestrator::InvalidInput(String::from("卷挂载路径无效")).into());
            }
            Ok(Volume {
                host_path: volume.host_path,
                container_path: volume.container_path,
                read_only: volume.read_only,
            })
        })
        .collect()
}

/// 转换并校验宿主机端口映射。
fn published_ports(input: Vec<super::proto::PublishedPort>) -> anyhow::Result<Vec<PublishedPort>> {
    input
        .into_iter()
        .map(|port| {
            Ok(PublishedPort {
                host_port: checked_port(port.host_port, "宿主机端口")?,
                container_port: checked_port(port.container_port, "容器端口")?,
                protocol: match PortProtocol::try_from(port.protocol)
                    .unwrap_or(PortProtocol::Unspecified)
                {
                    PortProtocol::Unspecified | PortProtocol::Tcp => generator::PortProtocol::Tcp,
                    PortProtocol::Udp => generator::PortProtocol::Udp,
                },
            })
        })
        .collect()
}

/// 规范化健康检查参数并填充默认值。
fn normalize_healthcheck(input: ProtoHealthcheckSpec) -> anyhow::Result<HealthcheckSpec> {
    if input.command.trim().is_empty() || input.command.contains(['\0', '\n', '\r']) {
        return Err(
            orchestrator::InvalidInput(String::from("健康检查命令不能为空或包含控制字符")).into(),
        );
    }
    let interval = if input.interval.is_empty() {
        String::from("30s")
    } else {
        input.interval
    };
    let timeout = if input.timeout.is_empty() {
        String::from("30s")
    } else {
        input.timeout
    };
    let retries = if input.retries == 0 { 3 } else { input.retries };
    for (label, value) in [
        ("健康检查间隔", interval.as_str()),
        ("健康检查超时", timeout.as_str()),
    ] {
        if value.contains(['\0', '\n', '\r', ' ']) {
            return Err(orchestrator::InvalidInput(format!("{label}格式无效: {value}")).into());
        }
    }
    if let Some(start_period) = &input.start_period
        && (start_period.contains(['\0', '\n', '\r', ' ']))
    {
        return Err(orchestrator::InvalidInput(format!(
            "健康检查启动宽限期格式无效: {start_period}"
        ))
        .into());
    }
    Ok(HealthcheckSpec {
        command: input.command,
        interval,
        timeout,
        start_period: input.start_period.filter(|value| !value.is_empty()),
        retries,
    })
}

/// 将基础设施协议请求转换为内部生成参数。
pub(super) fn infrastructure_spec(
    input: &InitializeInfrastructureRequest,
    configured_domain: &str,
) -> anyhow::Result<InfraSpec> {
    let http_port = if input.http_port == 0 {
        80
    } else {
        checked_port(input.http_port, "Traefik HTTP 宿主机端口")?
    };
    let https_port = if input.https_port == 0 {
        443
    } else {
        checked_port(input.https_port, "Traefik HTTPS 宿主机端口")?
    };
    let domain = if input.domain.is_empty() {
        configured_domain.to_string()
    } else {
        input.domain.clone()
    };
    crate::config::validate_domain(&domain)
        .map_err(|error| orchestrator::InvalidInput(error.to_string()))?;
    Ok(InfraSpec {
        domain,
        acme_email: input.acme_email.clone(),
        cloudflare_token: input.cloudflare_token.clone(),
        traefik_version: input.traefik_version.clone(),
        http_port,
        https_port,
    })
}

/// 将静态站点协议请求转换为内部生成参数。
pub(super) fn static_site_spec(
    input: CreateStaticSiteRequest,
    configured_domain: &str,
) -> anyhow::Result<StaticSiteSpec> {
    Ok(StaticSiteSpec {
        name: input.name,
        host: resolve_host(&input.host, configured_domain)?,
        assets: input
            .assets
            .into_iter()
            .map(|asset| StaticAsset {
                path: asset.path.into(),
                content: asset.content,
            })
            .collect(),
        middlewares: input
            .middlewares
            .into_iter()
            .map(middleware_from_proto)
            .collect::<anyhow::Result<Vec<_>>>()?,
        nginx_version: input.nginx_version,
    })
}

/// 将单个 DNS 标签扩展为全局主域名下的完整域名。
fn resolve_host(host: &str, configured_domain: &str) -> anyhow::Result<String> {
    if host.contains('.') {
        return Ok(host.to_string());
    }
    let valid = !host.is_empty()
        && host.len() <= 63
        && host
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && host
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && host
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if !valid {
        return Err(orchestrator::InvalidInput(format!("短子域名格式无效: {host}")).into());
    }
    Ok(format!("{host}.{configured_domain}"))
}

/// 将内部项目状态转换为协议响应。
pub(super) fn stack_to_proto(stack: StackInfo) -> Stack {
    Stack {
        name: stack.name,
        project_directory: stack.project_directory.display().to_string(),
        compose_file: stack.compose_file.display().to_string(),
        env_file: stack.env_file.display().to_string(),
        containers: stack
            .containers
            .into_iter()
            .map(|container| Container {
                service: container.service,
                name: container.name,
                state: match container.state {
                    InternalContainerState::Unknown => ContainerState::Unspecified,
                    InternalContainerState::Created => ContainerState::Created,
                    InternalContainerState::Running => ContainerState::Running,
                    InternalContainerState::Paused => ContainerState::Paused,
                    InternalContainerState::Restarting => ContainerState::Restarting,
                    InternalContainerState::Removing => ContainerState::Removing,
                    InternalContainerState::Exited => ContainerState::Exited,
                    InternalContainerState::Dead => ContainerState::Dead,
                }
                .into(),
                health: match container.health {
                    InternalContainerHealth::None => ContainerHealth::Unspecified,
                    InternalContainerHealth::Starting => ContainerHealth::Starting,
                    InternalContainerHealth::Healthy => ContainerHealth::Healthy,
                    InternalContainerHealth::Unhealthy => ContainerHealth::Unhealthy,
                    InternalContainerHealth::Unknown => ContainerHealth::Unknown,
                }
                .into(),
            })
            .collect(),
    }
}

/// 将生成参数错误转换为稳定的无效输入错误。
pub(super) fn invalid_generation(error: impl std::fmt::Display) -> anyhow::Error {
    orchestrator::InvalidInput(error.to_string()).into()
}

/// 构建成功操作响应。
pub(super) fn operation_response(message: String) -> Response<OperationResponse> {
    Response::new(OperationResponse {
        success: true,
        message,
    })
}

/// 将内部错误转换为稳定的 gRPC 状态码。
pub(super) fn status_from_error(error: &anyhow::Error) -> Status {
    if let Some(error) = error.downcast_ref::<orchestrator::InvalidInput>() {
        Status::invalid_argument(error.to_string())
    } else if let Some(error) = error.downcast_ref::<orchestrator::StackNotFound>() {
        Status::not_found(error.to_string())
    } else {
        Status::internal(error.to_string())
    }
}

/// 将协议中间件枚举转换为内部枚举。
fn middleware_from_proto(value: i32) -> anyhow::Result<Middleware> {
    match ApplicationMiddleware::try_from(value).unwrap_or(ApplicationMiddleware::Unspecified) {
        ApplicationMiddleware::Unspecified => {
            Err(orchestrator::InvalidInput(String::from("中间件类型不能为未指定")).into())
        }
        ApplicationMiddleware::Gzip => Ok(Middleware::Gzip),
        ApplicationMiddleware::ForwardedHeaders => Ok(Middleware::ForwardedHeaders),
        ApplicationMiddleware::InternalOnly => Ok(Middleware::InternalOnly),
    }
}

/// 将协议端口检查并收窄为 `u16`。
fn checked_port(value: u32, label: &str) -> anyhow::Result<u16> {
    u16::try_from(value)
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| orchestrator::InvalidInput(format!("{label}必须在 1..=65535 范围内")).into())
}
