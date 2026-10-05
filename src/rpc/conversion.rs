//! protobuf 线路表示与领域模型之间的转换和校验。

use super::proto;
use crate::orchestrator::{Edit, NetworkEdit, StackInfo};
use crate::spec::{BindMount, Healthcheck, PortProtocol, PublishedPort, Route, RouteProtocol};
use anyhow::Context;
use tonic::Status;

/// 将用户输入错误或操作错误转换为 RPC 状态。
pub(super) fn status_from_error(error: &anyhow::Error) -> Status {
    Status::invalid_argument(format!("{error:#}"))
}

/// 将管理器项目信息转换为线路表示。
pub(super) fn stack_to_proto(stack: StackInfo) -> proto::Stack {
    proto::Stack {
        name: stack.name,
        services: stack.services,
        compose_yaml: stack.compose_yaml,
        environment: stack.environment.into_iter().collect(),
        status: stack.status,
    }
}

/// 转换并校验线路修改表示。
///
/// # 错误
///
/// 枚举值无效或数值越界时返回错误。
pub(super) fn edit_from_proto(request: proto::EditRequest) -> anyhow::Result<Edit> {
    let routes = if request.routes.is_empty() {
        None
    } else {
        Some(
            request
                .routes
                .into_iter()
                .map(route_from_proto)
                .collect::<anyhow::Result<_>>()?,
        )
    };
    let published_ports = if request.published_ports.is_empty() {
        None
    } else {
        Some(
            request
                .published_ports
                .into_iter()
                .map(port_from_proto)
                .collect::<anyhow::Result<_>>()?,
        )
    };
    let volumes = request
        .volumes
        .into_iter()
        .map(|volume| {
            let mount = BindMount {
                host_path: volume.host_path,
                container_path: volume.container_path,
                read_only: volume.read_only,
            };
            BindMount::parse(&mount.compose_value())
        })
        .collect::<anyhow::Result<_>>()?;
    let middlewares = if request.middlewares.is_empty() {
        None
    } else {
        Some(middleware_names(&request.middlewares)?)
    };
    let network = request
        .network_mode
        .map(|value| network_from_proto(value, request.external_network.as_deref()))
        .transpose()?;
    let healthcheck = request
        .healthcheck
        .map(healthcheck_from_proto)
        .transpose()?;
    Ok(Edit {
        service: request.service,
        image: request.image,
        version: request.version,
        command: (!request.command.is_empty()).then_some(request.command),
        container_port: request
            .container_port
            .map(|value| u16::try_from(value).context("container_port 超出范围"))
            .transpose()?,
        routes,
        published_ports,
        volumes,
        environment: request.environment.into_iter().collect(),
        network,
        middlewares,
        labels: request.labels,
        healthcheck,
        remove_healthcheck: request.remove_healthcheck,
        start: request.start,
    })
}

/// 将一条线路路由转换为语义视图。
///
/// # 错误
///
/// 协议、中间件值或端口无效时返回错误。
fn route_from_proto(route: proto::Route) -> anyhow::Result<Route> {
    Ok(Route {
        name: route.name,
        hosts: route.hosts,
        path_prefix: route.path_prefix,
        container_port: Some(u16::try_from(route.container_port).context("路由端口超出范围")?),
        middlewares: middleware_names(&route.middlewares)?,
        protocol: match proto::RouteProtocol::try_from(route.protocol)? {
            proto::RouteProtocol::Unspecified | proto::RouteProtocol::Http => RouteProtocol::Http,
            proto::RouteProtocol::Https => RouteProtocol::Https,
            proto::RouteProtocol::H2c => RouteProtocol::H2c,
        },
        entrypoint: route.entrypoint.unwrap_or_default(),
        sticky_cookie: route.sticky_cookie,
        pass_host_header: route.pass_host_header,
        priority: route.priority,
        service: None,
        tls_domains: Vec::new(),
    })
}

/// 转换一条线路发布端口映射。
///
/// # 错误
///
/// 协议无效或数值越界时返回错误。
fn port_from_proto(port: proto::PublishedPort) -> anyhow::Result<PublishedPort> {
    let value = PublishedPort {
        host_ip: port.host_ip,
        host_port: u16::try_from(port.host_port).context("宿主机端口超出范围")?,
        container_port: u16::try_from(port.container_port).context("容器端口超出范围")?,
        protocol: match proto::PortProtocol::try_from(port.protocol)? {
            proto::PortProtocol::Unspecified | proto::PortProtocol::Tcp => PortProtocol::Tcp,
            proto::PortProtocol::Udp => PortProtocol::Udp,
        },
    };
    PublishedPort::parse(&value.compose_value())
}

/// 转换线路网络变更。
///
/// # 错误
///
/// 值未指定或缺少外部网络名时返回错误。
fn network_from_proto(value: i32, external: Option<&str>) -> anyhow::Result<NetworkEdit> {
    match proto::NetworkMode::try_from(value)? {
        proto::NetworkMode::Unspecified => anyhow::bail!("network_mode 不能为 unspecified"),
        proto::NetworkMode::Bridge => Ok(NetworkEdit::Bridge),
        proto::NetworkMode::Host => Ok(NetworkEdit::Host),
        proto::NetworkMode::External => Ok(NetworkEdit::External(
            external
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow::anyhow!("external 网络缺少 external_network"))?
                .to_string(),
        )),
    }
}

/// 校验路由或编辑请求引用的 Traefik 中间件名称。
///
/// 中间件不再限制为内置枚举：`authelia`、`gzip` 等由 traefik 模板生成，其它名称
/// 允许引用用户通过 `[traefik.middlewares]` 或 `files/` 追加的自定义中间件。
///
/// # 错误
///
/// 名称为空或包含无法出现在 Traefik label 中的字符时返回错误。
fn middleware_names(values: &[String]) -> anyhow::Result<Vec<String>> {
    for value in values {
        crate::spec::validate_middleware(value)?;
    }
    Ok(values.to_vec())
}

/// 将线路健康检查转换为 Compose IR 表示。
///
/// # 错误
///
/// 命令为空或重试次数无效时返回错误。
fn healthcheck_from_proto(value: proto::Healthcheck) -> anyhow::Result<Healthcheck> {
    if value.retries == Some(0) {
        anyhow::bail!("healthcheck retries 必须大于 0");
    }
    let mut output = match (value.command.is_empty(), value.exec.is_empty()) {
        (false, true) => Healthcheck::command(value.command),
        (true, false) => Healthcheck::exec(&value.exec)?,
        (false, false) => anyhow::bail!("healthcheck 不能同时使用 command 与 exec"),
        (true, true) => anyhow::bail!("healthcheck 需要 command 或 exec"),
    };
    output.interval = value.interval;
    output.timeout = value.timeout;
    output.start_period = value.start_period;
    output.retries = value.retries;
    output.validate()?;
    Ok(output)
}
