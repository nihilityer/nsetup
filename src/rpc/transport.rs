//! Unix socket 与 Bearer 认证 TCP 服务端传输。

use super::RpcService;
use super::proto::orchestrator_server::OrchestratorServer;
use crate::config::{Config, set_mode};
use crate::constants::{ADMIN_GROUP, MAX_RPC_MESSAGE_SIZE};
use crate::install::ensure_auth_token;
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Server;
use tonic::{Request, Status};

/// 启动配置的 gRPC 监听器并运行到收到终止信号。
///
/// # 错误
///
/// 监听器、权限、认证或服务器发生故障时返回错误。
pub async fn serve(config: Config) -> anyhow::Result<()> {
    if let Some(path) = config.listen.strip_prefix("unix://") {
        return serve_unix(config.clone(), PathBuf::from(path)).await;
    }
    serve_tcp(config).await
}

/// 通过受访问控制的 Unix 域套接字提供服务。
///
/// # 错误
///
/// 套接字准备或服务过程失败时返回错误。
async fn serve_unix(config: Config, socket: PathBuf) -> anyhow::Result<()> {
    prepare_socket(&socket)?;
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("无法绑定 gRPC socket: {}", socket.display()))?;
    set_mode(&socket, 0o660)?;
    let output = Command::new("chown")
        .arg(format!("root:{ADMIN_GROUP}"))
        .arg(&socket)
        .output()
        .context("无法设置 gRPC socket 属组")?;
    if !output.status.success() {
        // systemd 安装以 root 运行，属组必须设置成功；手工在普通用户下前台调试
        // daemon 时无权 chown，此时保留进程属主即可，不应让调试无法进行。
        if is_root()? {
            anyhow::bail!(
                "无法设置 gRPC socket 属组: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        tracing::warn!(
            "无法设置 gRPC socket 属组（当前非 root，仅本次调试）：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    tracing::info!("守护进程正在监听 unix://{}", socket.display());
    let result = Server::builder()
        .add_service(configured_server(RpcService::new(config)?))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown_signal())
        .await;
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    result?;
    Ok(())
}

/// 通过经过认证的 TCP 地址提供服务。
///
/// # 错误
///
/// 令牌加载、地址解析或服务过程失败时返回错误。
async fn serve_tcp(config: Config) -> anyhow::Result<()> {
    let address = config
        .listen
        .strip_prefix("tcp://")
        .unwrap_or(&config.listen)
        .parse()
        .context("gRPC TCP 监听地址无效")?;
    let token = ensure_auth_token()?;
    let server = configured_server(RpcService::new(config)?);
    let authenticated =
        InterceptedService::new(server, move |request| authenticate(request, &token));
    tracing::info!("守护进程正在监听 {address}");
    Server::builder()
        .add_service(authenticated)
        .serve_with_shutdown(address, shutdown_signal())
        .await?;
    Ok(())
}

/// 配置生成服务端的消息大小限制。
fn configured_server(service: RpcService) -> OrchestratorServer<RpcService> {
    OrchestratorServer::new(service)
        .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
        .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE)
}

/// 仅删除失效套接字，并创建其父目录。
///
/// # 错误
///
/// 遇到非套接字路径时返回错误，而不覆盖该路径。
fn prepare_socket(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let parent = socket
        .parent()
        .ok_or_else(|| anyhow::anyhow!("socket 缺少父目录: {}", socket.display()))?;
    std::fs::create_dir_all(parent)?;
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(socket)?,
        Ok(_) => anyhow::bail!("拒绝覆盖非 socket 路径: {}", socket.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// 判断当前进程的有效用户是否为 root。
///
/// # 错误
///
/// 无法执行 `id -u` 时返回错误。
fn is_root() -> anyhow::Result<bool> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("无法检查当前用户")?;
    if !output.status.success() {
        anyhow::bail!("id -u 失败，无法确认当前用户");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "0")
}

/// 使用固定工作量比较校验 TCP Bearer 元数据。
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
        Err(Status::unauthenticated("认证 token 无效或缺失"))
    }
}

/// 记录 TCP 认证成功的标记。
#[derive(Debug, Clone, Copy)]
struct Authenticated;

/// 比较密钥，且不根据内容提前返回。
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

/// 等待 Ctrl-C 或 Unix 终止信号。
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            if let Err(error) = ctrl_c.await {
                tracing::error!("无法监听 Ctrl-C: {error}");
            }
            return;
        };
        tokio::select! {
            result = ctrl_c => {
                if let Err(error) = result {
                    tracing::error!("无法监听 Ctrl-C: {error}");
                }
            }
            _signal = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = ctrl_c.await {
        tracing::error!("无法监听 Ctrl-C: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn secret_comparison_checks_length_and_content() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"taken"));
        assert!(!constant_time_eq(b"token", b"token-long"));
    }
}
