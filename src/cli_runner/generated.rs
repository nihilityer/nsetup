//! 基础设施与应用生成命令执行。

use crate::cli::generated::{
    AddArgs, AppCmd, AppParams, EditArgs, EnvironmentArg, InfraCmd, LabelArg, MiddlewareArg,
    MountArg, NamedVolumeArg, NetworkArg, PortMappingArg,
};
use crate::rpc::RpcClient;
use crate::rpc::proto::{
    ApplicationMiddleware, ApplicationNetworkMode, ApplicationRoute, ApplicationVolume,
    CreateApplicationRequest, CreateStaticSiteRequest, EnvironmentVariable, HealthcheckSpec,
    InitializeInfrastructureRequest, NamedVolume, PortProtocol, PublishedPort, StaticAsset,
    UpdateApplicationRequest,
};
use anyhow::Context;
use std::path::Path;

/// 执行基础设施生成命令。
pub(super) async fn run_infra(action: InfraCmd) -> anyhow::Result<()> {
    match action {
        InfraCmd::Init {
            domain,
            acme_email,
            cloudflare_token_file,
            traefik_version,
            http_port,
            https_port,
            start,
            force,
        } => {
            let input = InitializeInfrastructureRequest {
                domain: domain.unwrap_or_default(),
                acme_email,
                cloudflare_token: read_secret(&cloudflare_token_file)?,
                traefik_version,
                http_port: u32::from(http_port),
                https_port: u32::from(https_port),
                start,
                force,
            };
            let mut client = RpcClient::connect(None, None).await?;
            tracing::info!("{}", client.initialize_infrastructure(input).await?.message);
        }
    }
    Ok(())
}

/// 执行应用生成命令。
pub(super) async fn run_app(action: AppCmd) -> anyhow::Result<()> {
    match action {
        AppCmd::Add(arguments) => {
            let AddArgs {
                name,
                image,
                version,
                service,
                params,
                join,
                start,
                force,
            } = *arguments;
            let AppParams {
                command,
                container_port,
                hosts,
                routes,
                path_prefix,
                tcp_ports,
                udp_ports,
                volumes,
                read_only_volumes,
                environment,
                network,
                external_network,
                middleware,
                labels,
                named_volumes,
                healthcheck_cmd,
                healthcheck_interval,
                healthcheck_timeout,
                healthcheck_start_period,
                healthcheck_retries,
            } = params;
            let container_port = container_port.unwrap_or(80);
            let service = match (join, service) {
                (true, None) => anyhow::bail!("追加服务时必须指定 --service"),
                (_, service) => service.unwrap_or_else(|| String::from("app")),
            };
            let (network_mode, external_network) =
                network_input(network.unwrap_or(NetworkArg::Bridge), external_network)?;
            let input = CreateApplicationRequest {
                name,
                service,
                image,
                version,
                command,
                container_port: u32::from(container_port),
                routes: hosts
                    .into_iter()
                    .map(|host| ApplicationRoute {
                        host,
                        path_prefix: path_prefix.clone().unwrap_or_default(),
                        container_port: u32::from(container_port),
                    })
                    .chain(routes.into_iter().map(|route| ApplicationRoute {
                        host: route.host,
                        path_prefix: path_prefix.clone().unwrap_or_default(),
                        container_port: u32::from(route.container_port),
                    }))
                    .collect(),
                published_ports: published_ports(tcp_ports, udp_ports),
                volumes: application_volumes(volumes, read_only_volumes),
                environment: application_environment(environment),
                network_mode: network_mode.into(),
                external_network,
                middlewares: middleware.into_iter().map(middleware_input).collect(),
                labels: application_labels(labels),
                named_volumes: application_named_volumes(named_volumes),
                healthcheck: healthcheck_input(
                    &healthcheck_cmd,
                    &healthcheck_interval,
                    &healthcheck_timeout,
                    &healthcheck_start_period,
                    healthcheck_retries,
                )?,
                join,
                start,
                force,
            };
            let mut client = RpcClient::connect(None, None).await?;
            tracing::info!("{}", client.create_application(input).await?.message);
        }
        AppCmd::Edit(arguments) => run_edit(*arguments).await?,
        AppCmd::AddStatic {
            name,
            source,
            host,
            middleware,
            nginx_version,
            start,
            force,
        } => {
            let input = CreateStaticSiteRequest {
                name,
                host,
                assets: collect_assets(&source)?,
                middlewares: middleware.into_iter().map(middleware_input).collect(),
                nginx_version,
                start,
                force,
            };
            let mut client = RpcClient::connect(None, None).await?;
            tracing::info!("{}", client.create_static_site(input).await?.message);
        }
    }
    Ok(())
}

/// 执行参数化应用修改；未提供的参数保持原样。
async fn run_edit(arguments: EditArgs) -> anyhow::Result<()> {
    let EditArgs {
        name,
        compose,
        env_file,
        service,
        image,
        version,
        params,
        start,
        no_healthcheck,
    } = arguments;
    if let Some(compose) = compose {
        let compose_yaml = std::fs::read_to_string(&compose)
            .with_context(|| format!("无法读取 Compose 文件 {}", compose.display()))?;
        let env_content = env_file
            .as_deref()
            .map(std::fs::read_to_string)
            .transpose()
            .context("无法读取环境变量文件")?;
        let mut client = RpcClient::connect(None, None).await?;
        tracing::info!(
            "{}",
            client
                .update(name, compose_yaml, env_content, start)
                .await?
                .message
        );
        return Ok(());
    }
    if !edit_has_changes(
        &params,
        service.is_some(),
        image.is_some(),
        version.is_some(),
        no_healthcheck,
    ) {
        anyhow::bail!("未提供任何修改参数或 --compose");
    }
    let AppParams {
        command,
        container_port,
        hosts,
        routes,
        path_prefix,
        tcp_ports,
        udp_ports,
        volumes,
        read_only_volumes,
        environment,
        network,
        external_network,
        middleware,
        labels,
        named_volumes,
        healthcheck_cmd,
        healthcheck_interval,
        healthcheck_timeout,
        healthcheck_start_period,
        healthcheck_retries,
    } = params;
    let (network_mode, external_network) = match (network, external_network) {
        (Some(network), external) => {
            let (mode, name) = network_input(network, external)?;
            (Some(mode), (!name.is_empty()).then_some(name))
        }
        (None, Some(_)) => anyhow::bail!("--external-network 只能与 --network external 一起使用"),
        (None, None) => (None, None),
    };
    let healthcheck = healthcheck_input(
        &healthcheck_cmd,
        &healthcheck_interval,
        &healthcheck_timeout,
        &healthcheck_start_period,
        healthcheck_retries,
    )?;
    if no_healthcheck && healthcheck.is_some() {
        anyhow::bail!("--no-healthcheck 不能与健康检查参数同时使用");
    }
    let input = UpdateApplicationRequest {
        name,
        service,
        image,
        version,
        command,
        container_port: container_port.map(u32::from),
        routes: routes
            .into_iter()
            .map(|route| ApplicationRoute {
                host: route.host,
                path_prefix: String::new(),
                container_port: u32::from(route.container_port),
            })
            .collect(),
        published_ports: published_ports(tcp_ports, udp_ports),
        volumes: application_volumes(volumes, read_only_volumes),
        environment: application_environment(environment),
        network_mode: network_mode.map(Into::into),
        external_network,
        middlewares: middleware.into_iter().map(middleware_input).collect(),
        labels: application_labels(labels),
        named_volumes: application_named_volumes(named_volumes),
        path_prefix,
        hosts,
        healthcheck,
        remove_healthcheck: no_healthcheck,
        start,
    };
    let mut client = RpcClient::connect(None, None).await?;
    tracing::info!("{}", client.update_application(input).await?.message);
    Ok(())
}

/// 判断参数化编辑是否提供了任何修改。
const fn edit_has_changes(
    params: &AppParams,
    service: bool,
    image: bool,
    version: bool,
    no_healthcheck: bool,
) -> bool {
    service
        || image
        || version
        || no_healthcheck
        || !params.command.is_empty()
        || params.container_port.is_some()
        || !params.hosts.is_empty()
        || !params.routes.is_empty()
        || params.path_prefix.is_some()
        || !params.tcp_ports.is_empty()
        || !params.udp_ports.is_empty()
        || !params.volumes.is_empty()
        || !params.read_only_volumes.is_empty()
        || !params.environment.is_empty()
        || params.network.is_some()
        || params.external_network.is_some()
        || !params.middleware.is_empty()
        || !params.labels.is_empty()
        || !params.named_volumes.is_empty()
        || params.healthcheck_cmd.is_some()
        || params.healthcheck_interval.is_some()
        || params.healthcheck_timeout.is_some()
        || params.healthcheck_start_period.is_some()
        || params.healthcheck_retries.is_some()
}

/// 组装宿主机端口映射列表。
fn published_ports(tcp: Vec<PortMappingArg>, udp: Vec<PortMappingArg>) -> Vec<PublishedPort> {
    let mut ports = tcp
        .into_iter()
        .map(|port| PublishedPort {
            host_port: u32::from(port.host),
            container_port: u32::from(port.container),
            protocol: PortProtocol::Tcp.into(),
        })
        .collect::<Vec<_>>();
    ports.extend(udp.into_iter().map(|port| PublishedPort {
        host_port: u32::from(port.host),
        container_port: u32::from(port.container),
        protocol: PortProtocol::Udp.into(),
    }));
    ports
}

/// 组装应用卷挂载列表。
fn application_volumes(volumes: Vec<MountArg>, read_only: Vec<MountArg>) -> Vec<ApplicationVolume> {
    let mut mapped = volumes
        .into_iter()
        .map(|volume| ApplicationVolume {
            host_path: volume.host,
            container_path: volume.container,
            read_only: false,
        })
        .collect::<Vec<_>>();
    mapped.extend(read_only.into_iter().map(|volume| ApplicationVolume {
        host_path: volume.host,
        container_path: volume.container,
        read_only: true,
    }));
    mapped
}

/// 组装环境变量列表。
fn application_environment(environment: Vec<EnvironmentArg>) -> Vec<EnvironmentVariable> {
    environment
        .into_iter()
        .map(|variable| EnvironmentVariable {
            key: variable.key,
            value: variable.value,
        })
        .collect()
}

/// 组装自定义标签列表。
fn application_labels(labels: Vec<LabelArg>) -> Vec<String> {
    labels
        .into_iter()
        .map(|label| format!("{}={}", label.key, label.value))
        .collect()
}

/// 组装命名卷列表。
fn application_named_volumes(named_volumes: Vec<NamedVolumeArg>) -> Vec<NamedVolume> {
    named_volumes
        .into_iter()
        .map(|volume| NamedVolume {
            name: volume.name,
            container_path: volume.container,
        })
        .collect()
}

/// 组装健康检查参数；未提供命令时不启用健康检查。
fn healthcheck_input(
    cmd: &Option<String>,
    interval: &Option<String>,
    timeout: &Option<String>,
    start_period: &Option<String>,
    retries: Option<u32>,
) -> anyhow::Result<Option<HealthcheckSpec>> {
    let specified = cmd.is_some()
        || interval.is_some()
        || timeout.is_some()
        || start_period.is_some()
        || retries.is_some();
    if !specified {
        return Ok(None);
    }
    let command = cmd
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("指定健康检查参数时必须提供 --healthcheck-cmd"))?;
    if command.trim().is_empty() || command.contains(['\0', '\n', '\r']) {
        anyhow::bail!("健康检查命令不能为空或包含控制字符");
    }
    Ok(Some(HealthcheckSpec {
        command: command.clone(),
        interval: interval.clone().unwrap_or_default(),
        timeout: timeout.clone().unwrap_or_default(),
        start_period: start_period.clone(),
        retries: retries.unwrap_or_default(),
    }))
}

/// 将 CLI 网络枚举转换为协议枚举。
fn network_input(
    network: NetworkArg,
    external: Option<String>,
) -> anyhow::Result<(ApplicationNetworkMode, String)> {
    match (network, external) {
        (NetworkArg::Bridge, None) => Ok((ApplicationNetworkMode::Bridge, String::new())),
        (NetworkArg::Host, None) => Ok((ApplicationNetworkMode::Host, String::new())),
        (NetworkArg::External, Some(name)) if !name.is_empty() => {
            Ok((ApplicationNetworkMode::External, name))
        }
        (NetworkArg::External, None) => {
            anyhow::bail!("--network external 必须同时指定 --external-network")
        }
        (_, Some(_)) => anyhow::bail!("--external-network 只能与 --network external 一起使用"),
    }
}

/// 将 CLI 中间件枚举转换为协议枚举值。
fn middleware_input(middleware: MiddlewareArg) -> i32 {
    match middleware {
        MiddlewareArg::Gzip => ApplicationMiddleware::Gzip,
        MiddlewareArg::ForwardedHeaders => ApplicationMiddleware::ForwardedHeaders,
        MiddlewareArg::InternalOnly => ApplicationMiddleware::InternalOnly,
    }
    .into()
}

/// 读取并裁剪密钥文件末尾换行。
fn read_secret(path: &Path) -> anyhow::Result<String> {
    let value = std::fs::read_to_string(path)
        .with_context(|| format!("无法读取密钥文件: {}", path.display()))?;
    let value = value.trim_end_matches(['\n', '\r']).to_string();
    if value.is_empty() {
        anyhow::bail!("密钥文件不能为空: {}", path.display());
    }
    Ok(value)
}

/// 收集静态站点目录中的普通文件。
fn collect_assets(root: &Path) -> anyhow::Result<Vec<StaticAsset>> {
    let metadata = std::fs::symlink_metadata(root)
        .with_context(|| format!("无法读取静态文件目录: {}", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("静态文件来源必须是普通目录: {}", root.display());
    }
    let mut assets = Vec::new();
    collect_directory(root, root, &mut assets, &mut 0)?;
    if assets.is_empty() {
        anyhow::bail!("静态文件目录不能为空: {}", root.display());
    }
    Ok(assets)
}

/// 递归收集目录并实施文件数量和总大小限制。
fn collect_directory(
    root: &Path,
    directory: &Path,
    assets: &mut Vec<StaticAsset>,
    total_size: &mut u64,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            anyhow::bail!("静态文件目录不能包含符号链接: {}", entry.path().display());
        }
        let metadata = entry.metadata()?;
        if file_type.is_dir() {
            collect_directory(root, &entry.path(), assets, total_size)?;
        } else if file_type.is_file() {
            *total_size += metadata.len();
            if assets.len() >= 10_000 || *total_size > 64 * 1024 * 1024 {
                anyhow::bail!("静态站点最多包含 10000 个文件且总大小不能超过 64 MiB");
            }
            assets.push(StaticAsset {
                path: entry
                    .path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .to_string(),
                content: std::fs::read(entry.path())?,
            });
        }
    }
    Ok(())
}
