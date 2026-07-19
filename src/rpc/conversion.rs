//! gRPC 类型与内部生成模型的转换。

use super::proto::{
    ApplicationMiddleware, ApplicationNetworkMode, Container, ContainerHealth, ContainerState,
    CreateApplicationRequest, CreateStaticSiteRequest, InitializeInfrastructureRequest,
    OperationResponse, PortProtocol, Stack,
};
use crate::generator::{
    self, AppSpec, InfraSpec, Middleware, NetworkMode, PublishedPort, Route, StaticAsset,
    StaticSiteSpec, Volume,
};
use crate::orchestrator::{
    self, ContainerHealth as InternalContainerHealth, ContainerState as InternalContainerState,
    StackInfo,
};
use tonic::{Response, Status};

/// 将常规应用协议请求转换为内部生成参数。
pub(super) fn application_spec(input: CreateApplicationRequest) -> anyhow::Result<AppSpec> {
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
    let mut environment = std::collections::BTreeMap::new();
    for variable in input.environment {
        if environment
            .insert(variable.key.clone(), variable.value)
            .is_some()
        {
            return Err(
                orchestrator::InvalidInput(format!("环境变量重复: {}", variable.key)).into(),
            );
        }
    }
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
                    host: route.host,
                    path_prefix: (!route.path_prefix.is_empty()).then_some(route.path_prefix),
                    container_port: if route.container_port == 0 {
                        checked_port(input.container_port, "容器端口")?
                    } else {
                        checked_port(route.container_port, "路由容器端口")?
                    },
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        published_ports: input
            .published_ports
            .into_iter()
            .map(|port| {
                Ok(PublishedPort {
                    host_port: checked_port(port.host_port, "宿主机端口")?,
                    container_port: checked_port(port.container_port, "容器端口")?,
                    protocol: match PortProtocol::try_from(port.protocol)
                        .unwrap_or(PortProtocol::Unspecified)
                    {
                        PortProtocol::Unspecified | PortProtocol::Tcp => {
                            generator::PortProtocol::Tcp
                        }
                        PortProtocol::Udp => generator::PortProtocol::Udp,
                    },
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        volumes: input
            .volumes
            .into_iter()
            .map(|volume| Volume {
                host_path: volume.host_path,
                container_path: volume.container_path,
                read_only: volume.read_only,
            })
            .collect(),
        environment,
        network_mode,
        middlewares: input
            .middlewares
            .into_iter()
            .map(middleware_from_proto)
            .collect::<anyhow::Result<Vec<_>>>()?,
    })
}

/// 将基础设施协议请求转换为内部生成参数。
pub(super) fn infrastructure_spec(input: &InitializeInfrastructureRequest) -> InfraSpec {
    InfraSpec {
        domain: input.domain.clone(),
        acme_email: input.acme_email.clone(),
        cloudflare_token: input.cloudflare_token.clone(),
        traefik_version: input.traefik_version.clone(),
    }
}

/// 将静态站点协议请求转换为内部生成参数。
pub(super) fn static_site_spec(input: CreateStaticSiteRequest) -> anyhow::Result<StaticSiteSpec> {
    Ok(StaticSiteSpec {
        name: input.name,
        host: input.host,
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
