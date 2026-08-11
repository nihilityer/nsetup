//! 基础设施与应用生成命令执行。

use crate::cli::generated::{AddArgs, AppCmd, InfraCmd, MiddlewareArg, NetworkArg};
use crate::rpc::RpcClient;
use crate::rpc::proto::{
    ApplicationMiddleware, ApplicationNetworkMode, ApplicationRoute, ApplicationVolume,
    CreateApplicationRequest, CreateStaticSiteRequest, EnvironmentVariable,
    InitializeInfrastructureRequest, NamedVolume, PortProtocol, PublishedPort, StaticAsset,
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
                start,
                force,
            } = *arguments;
            let mut published_ports = tcp_ports
                .into_iter()
                .map(|port| PublishedPort {
                    host_port: u32::from(port.host),
                    container_port: u32::from(port.container),
                    protocol: PortProtocol::Tcp.into(),
                })
                .collect::<Vec<_>>();
            published_ports.extend(udp_ports.into_iter().map(|port| PublishedPort {
                host_port: u32::from(port.host),
                container_port: u32::from(port.container),
                protocol: PortProtocol::Udp.into(),
            }));
            let mut mapped_volumes = volumes
                .into_iter()
                .map(|volume| ApplicationVolume {
                    host_path: volume.host,
                    container_path: volume.container,
                    read_only: false,
                })
                .collect::<Vec<_>>();
            mapped_volumes.extend(
                read_only_volumes
                    .into_iter()
                    .map(|volume| ApplicationVolume {
                        host_path: volume.host,
                        container_path: volume.container,
                        read_only: true,
                    }),
            );
            let (network_mode, external_network) = network_input(network, external_network)?;
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
                published_ports,
                volumes: mapped_volumes,
                environment: environment
                    .into_iter()
                    .map(|variable| EnvironmentVariable {
                        key: variable.key,
                        value: variable.value,
                    })
                    .collect(),
                network_mode: network_mode.into(),
                external_network,
                middlewares: middleware.into_iter().map(middleware_input).collect(),
                labels: labels
                    .into_iter()
                    .map(|label| format!("{}={}", label.key, label.value))
                    .collect(),
                named_volumes: named_volumes
                    .into_iter()
                    .map(|volume| NamedVolume {
                        name: volume.name,
                        container_path: volume.container,
                    })
                    .collect(),
                start,
                force,
            };
            let mut client = RpcClient::connect(None, None).await?;
            tracing::info!("{}", client.create_application(input).await?.message);
        }
        AppCmd::Edit {
            name,
            compose,
            env_file,
            start,
        } => {
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
        }
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
