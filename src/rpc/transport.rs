//! gRPC Unix/TCP 服务传输与认证。

use super::MAX_RPC_MESSAGE_SIZE;
use super::proto::orchestrator_server::OrchestratorServer;
use super::service::OrchestratorService;
use crate::config::{Config, ensure_auth_token};
use anyhow::Context;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Server;
use tonic::{Request, Status};
use tracing::info;

/// 启动 gRPC 守护进程并等待系统终止信号。
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let unix_socket = config
        .grpc
        .listen
        .strip_prefix("unix://")
        .map(PathBuf::from);
    if let Some(path) = unix_socket {
        return serve_unix(config, path).await;
    }
    serve_tcp(config).await
}

/// 在 Unix domain socket 上启动仅受文件权限保护的本机服务。
async fn serve_unix(config: Config, socket: PathBuf) -> anyhow::Result<()> {
    prepare_socket(&socket)?;
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("无法绑定 gRPC socket: {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;
    info!("Nihility 机器守护进程正在监听 unix://{}", socket.display());
    info!("Compose 项目目录: {}", config.paths.apps_root.display());
    let result = Server::builder()
        .add_service(configured_server(config))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown_signal())
        .await;
    if socket.exists() {
        std::fs::remove_file(&socket)
            .with_context(|| format!("无法清理 gRPC socket: {}", socket.display()))?;
    }
    result?;
    info!("Nihility 机器守护进程已停止");
    Ok(())
}

/// 在 TCP 地址上启动使用 Bearer 令牌认证的远程服务。
async fn serve_tcp(config: Config) -> anyhow::Result<()> {
    let listen = config.grpc.listen.trim_start_matches("tcp://");
    let address = listen
        .parse()
        .map_err(|error| anyhow::anyhow!("gRPC 监听地址无效 {listen}: {error}"))?;
    let token = ensure_auth_token()?;
    let service = OrchestratorService::new(config.clone());
    let server = OrchestratorServer::new(service)
        .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
        .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE);
    let authenticated =
        InterceptedService::new(server, move |request| authenticate(request, &token));
    info!("Nihility 机器守护进程正在监听 {address}");
    info!("Compose 项目目录: {}", config.paths.apps_root.display());
    Server::builder()
        .add_service(authenticated)
        .serve_with_shutdown(address, shutdown_signal())
        .await?;
    info!("Nihility 机器守护进程已停止");
    Ok(())
}

/// 构造支持静态站点上传大小限制的本机服务。
fn configured_server(config: Config) -> OrchestratorServer<OrchestratorService> {
    OrchestratorServer::new(OrchestratorService::new(config))
        .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
        .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE)
}

/// 创建 socket 父目录，并拒绝覆盖普通文件或符号链接。
fn prepare_socket(socket: &Path) -> anyhow::Result<()> {
    let parent = socket
        .parent()
        .ok_or_else(|| anyhow::anyhow!("gRPC socket 缺少父目录: {}", socket.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("无法创建运行目录: {}", parent.display()))?;
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            std::fs::remove_file(socket)?;
        }
        Ok(_) => anyhow::bail!("拒绝覆盖非 socket 路径: {}", socket.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// 校验 TCP 请求中的 Bearer 令牌。
fn authenticate(mut request: Request<()>, expected: &str) -> Result<Request<()>, Status> {
    let actual = request
        .metadata()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if actual.is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes())) {
        request.extensions_mut().insert(Authenticated);
        Ok(request)
    } else {
        Err(Status::unauthenticated("认证令牌无效或缺失"))
    }
}

/// 已认证 TCP 请求的内部标记。
#[derive(Debug, Clone, Copy)]
struct Authenticated;

/// 使用固定循环比较敏感字符串，减少提前返回造成的时序差异。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left_byte = left.get(index).copied().unwrap_or_default();
        let right_byte = right.get(index).copied().unwrap_or_default();
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

/// 等待 Ctrl-C 或 systemd 发出的终止信号。
async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!("无法监听终止信号: {}", error);
    }
}
