//! 将 CLI 编辑参数转换为类型化 protobuf 请求。

use super::args::{EditArgs, MiddlewareArg, NetworkArg, ProtocolArg};
use crate::rpc::proto;
use crate::spec::{BindMount, PortProtocol, PublishedPort};
use std::collections::HashMap;

/// 将 CLI 编辑标志转换为类型化协议请求。
///
/// # 错误
///
/// 端口、挂载、环境变量或标志组合格式错误时返回错误。
pub(super) fn edit_request(args: EditArgs) -> anyhow::Result<proto::EditRequest> {
    if args.hosts.is_empty()
        && (args.path_prefix.is_some()
            || args.sticky_cookie
            || args.pass_host_header.is_some()
            || args.priority.is_some())
    {
        anyhow::bail!("路由专用参数需要至少一个 --host");
    }
    let middleware_values: Vec<i32> = args
        .middlewares
        .iter()
        .copied()
        .map(middleware_value)
        .collect();
    let routes = if args.hosts.is_empty() {
        Vec::new()
    } else {
        vec![proto::Route {
            name: String::from("default"),
            hosts: args.hosts,
            path_prefix: args.path_prefix,
            container_port: u32::from(
                args.container_port
                    .ok_or_else(|| anyhow::anyhow!("新增 --host 时必须提供 --port"))?,
            ),
            middlewares: middleware_values.clone(),
            protocol: protocol_value(args.protocol),
            sticky_cookie: args.sticky_cookie,
            pass_host_header: args.pass_host_header,
            priority: args.priority,
        }]
    };
    let published_ports = args
        .published_ports
        .iter()
        .map(|value| PublishedPort::parse(value).map(port_to_proto))
        .collect::<anyhow::Result<_>>()?;
    let volumes = args
        .volumes
        .iter()
        .map(|value| BindMount::parse(value).map(volume_to_proto))
        .collect::<anyhow::Result<_>>()?;
    let environment = parse_pairs(&args.environment, "环境变量")?;
    validate_pairs(&args.labels, "Docker label")?;
    let network_mode = args.network.map(network_value);
    if matches!(args.network, Some(NetworkArg::External)) && args.external_network.is_none() {
        anyhow::bail!("--network external 必须同时提供 --external-network");
    }
    if !matches!(args.network, Some(NetworkArg::External)) && args.external_network.is_some() {
        anyhow::bail!("--external-network 只能与 --network external 一起使用");
    }
    let health_fields_present = args.healthcheck_interval.is_some()
        || args.healthcheck_timeout.is_some()
        || args.healthcheck_start_period.is_some()
        || args.healthcheck_retries.is_some();
    if health_fields_present && args.healthcheck_cmd.is_none() {
        anyhow::bail!("健康检查参数需要 --healthcheck-cmd");
    }
    let healthcheck = args.healthcheck_cmd.map(|command| proto::Healthcheck {
        command,
        interval: args.healthcheck_interval,
        timeout: args.healthcheck_timeout,
        start_period: args.healthcheck_start_period,
        retries: args.healthcheck_retries,
    });
    Ok(proto::EditRequest {
        name: args.name,
        service: args.service,
        image: args.image,
        version: args.version,
        command: args.command,
        container_port: args.container_port.map(u32::from),
        routes,
        published_ports,
        volumes,
        environment,
        network_mode,
        external_network: args.external_network,
        middlewares: middleware_values,
        labels: args.labels,
        healthcheck,
        remove_healthcheck: args.remove_healthcheck,
        start: args.start,
    })
}

/// 将语义端口映射转换为 protobuf 表示。
fn port_to_proto(value: PublishedPort) -> proto::PublishedPort {
    proto::PublishedPort {
        host_ip: value.host_ip,
        host_port: u32::from(value.host_port),
        container_port: u32::from(value.container_port),
        protocol: match value.protocol {
            PortProtocol::Tcp => proto::PortProtocol::Tcp as i32,
            PortProtocol::Udp => proto::PortProtocol::Udp as i32,
        },
    }
}

/// 将语义 bind mount 转换为 protobuf 表示。
fn volume_to_proto(value: BindMount) -> proto::Volume {
    proto::Volume {
        host_path: value.host_path,
        container_path: value.container_path,
        read_only: value.read_only,
    }
}

/// 将 CLI 网络选项转换为 protobuf 数值。
const fn network_value(value: NetworkArg) -> i32 {
    match value {
        NetworkArg::Bridge => proto::NetworkMode::Bridge as i32,
        NetworkArg::Host => proto::NetworkMode::Host as i32,
        NetworkArg::External => proto::NetworkMode::External as i32,
    }
}

/// 将 CLI 协议选项转换为 protobuf 数值。
const fn protocol_value(value: ProtocolArg) -> i32 {
    match value {
        ProtocolArg::Http => proto::RouteProtocol::Http as i32,
        ProtocolArg::Https => proto::RouteProtocol::Https as i32,
        ProtocolArg::H2c => proto::RouteProtocol::H2c as i32,
    }
}

/// 将 CLI 中间件选项转换为 protobuf 数值。
const fn middleware_value(value: MiddlewareArg) -> i32 {
    match value {
        MiddlewareArg::Authelia => proto::Middleware::Authelia as i32,
        MiddlewareArg::Gzip => proto::Middleware::Gzip as i32,
        MiddlewareArg::ForwardedHeaders => proto::Middleware::ForwardedHeaders as i32,
        MiddlewareArg::InternalOnly => proto::Middleware::InternalOnly as i32,
        MiddlewareArg::Tls => proto::Middleware::Tls as i32,
    }
}

/// 将重复的 `KEY=VALUE` CLI 参数解析为 protobuf 映射。
///
/// # 错误
///
/// 缺少分隔符、键为空或键重复时返回错误。
fn parse_pairs(values: &[String], label: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut output = HashMap::new();
    for value in values {
        let (key, content) = value
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("{label}必须是 KEY=VALUE: {value}"))?;
        if key.is_empty() {
            anyhow::bail!("{label}键不能为空");
        }
        if output
            .insert(key.to_string(), content.to_string())
            .is_some()
        {
            anyhow::bail!("{label}重复: {key}");
        }
    }
    Ok(output)
}

/// 校验重复的 `KEY=VALUE` 参数并保留原始顺序。
///
/// # 错误
///
/// 键格式错误或重复时返回错误。
fn validate_pairs(values: &[String], label: &str) -> anyhow::Result<()> {
    let _pairs = parse_pairs(values, label)?;
    Ok(())
}
