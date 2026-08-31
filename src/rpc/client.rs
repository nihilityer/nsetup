//! 同时支持 Unix socket 与认证 TCP 的 RPC 客户端。

use super::proto;
use crate::constants::{GRPC_SOCKET, MAX_RPC_MESSAGE_SIZE};
use anyhow::Context;
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use tokio::net::UnixStream;
use tonic::Request;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

use proto::orchestrator_client::OrchestratorClient;

/// 同时支持 Unix socket 与认证 TCP 通道的客户端。
#[derive(Debug, Clone)]
pub struct RpcClient {
    /// 生成的 Tonic 客户端。
    inner: OrchestratorClient<Channel>,
    /// 可选的 TCP 认证元数据。
    authorization: Option<MetadataValue<Ascii>>,
}

impl RpcClient {
    /// 连接所选守护进程端点。
    ///
    /// # 错误
    ///
    /// 传输失败或缺少 TCP 凭据时返回错误。
    pub async fn connect(
        endpoint: Option<&str>,
        token_file: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let endpoint = endpoint
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("unix://{GRPC_SOCKET}"));
        if let Some(path) = endpoint.strip_prefix("unix://") {
            if token_file.is_some() {
                anyhow::bail!("Unix socket 连接不使用 --token-file");
            }
            return Self::connect_unix(path).await;
        }
        Self::connect_tcp(endpoint, token_file).await
    }

    /// 调用状态 RPC。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn status(&mut self) -> anyhow::Result<proto::StatusResponse> {
        let request = self.request(proto::StatusRequest {});
        Ok(self.inner.status(request).await?.into_inner())
    }

    /// 应用 TOML 声明。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn apply(
        &mut self,
        request: proto::ApplyRequest,
    ) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(request);
        Ok(self.inner.apply(request).await?.into_inner())
    }

    /// 导入 Compose 状态。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn import_compose(
        &mut self,
        request: proto::ImportComposeRequest,
    ) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(request);
        Ok(self.inner.import_compose(request).await?.into_inner())
    }

    /// 导出当前项目状态。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn export(&mut self, name: String) -> anyhow::Result<proto::ExportResponse> {
        let request = self.request(proto::ExportRequest { name });
        Ok(self.inner.export(request).await?.into_inner())
    }

    /// 应用部分服务修改。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn edit(
        &mut self,
        edit: proto::EditRequest,
    ) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(edit);
        Ok(self.inner.edit(request).await?.into_inner())
    }

    /// 列出受管项目。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn list(&mut self) -> anyhow::Result<proto::ListResponse> {
        let request = self.request(proto::ListRequest {});
        Ok(self.inner.list(request).await?.into_inner())
    }

    /// 获取一个受管项目。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn get(&mut self, name: String) -> anyhow::Result<proto::Stack> {
        let request = self.request(proto::GetRequest { name });
        Ok(self.inner.get(request).await?.into_inner())
    }

    /// 删除已经确认的项目。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn remove(&mut self, name: String) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(proto::RemoveRequest { name, force: true });
        Ok(self.inner.remove(request).await?.into_inner())
    }

    /// 调用非流式生命周期 RPC。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn action(
        &mut self,
        name: String,
        action: Action,
    ) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(proto::ActionRequest { name });
        let response = match action {
            Action::Start => self.inner.start(request).await?,
            Action::Stop => self.inner.stop(request).await?,
            Action::Restart => self.inner.restart(request).await?,
            Action::Build => self.inner.build(request).await?,
        };
        Ok(response.into_inner())
    }

    /// 发起服务端流式镜像拉取。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn pull(
        &mut self,
        name: String,
    ) -> anyhow::Result<tonic::Streaming<proto::PullProgress>> {
        let request = self.request(proto::ActionRequest { name });
        Ok(self.inner.pull(request).await?.into_inner())
    }

    /// 发起服务端流式日志调用。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn logs(
        &mut self,
        name: String,
        tail: u32,
        follow: bool,
    ) -> anyhow::Result<tonic::Streaming<proto::LogLine>> {
        let request = self.request(proto::LogsRequest { name, tail, follow });
        Ok(self.inner.logs(request).await?.into_inner())
    }

    /// 通过 Unix 域套接字连接 Tonic 通道。
    ///
    /// # 错误
    ///
    /// 无法连接套接字时返回错误。
    async fn connect_unix(path: &str) -> anyhow::Result<Self> {
        let socket = PathBuf::from(path);
        let connector = socket.clone();
        let channel = Endpoint::try_from("http://[::]:50051")?
            .connect_with_connector(service_fn(move |_| {
                let path = connector.clone();
                async move { UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .with_context(|| format!("无法连接本机 gRPC socket: {}", socket.display()))?;
        Ok(Self {
            inner: configured_client(channel),
            authorization: None,
        })
    }

    /// 连接经过认证的 TCP Tonic 通道。
    ///
    /// # 错误
    ///
    /// 令牌或端点无效时返回错误。
    async fn connect_tcp(endpoint: String, token_file: Option<&Path>) -> anyhow::Result<Self> {
        let token_path =
            token_file.ok_or_else(|| anyhow::anyhow!("TCP 连接必须提供 --token-file"))?;
        let token = std::fs::read_to_string(token_path)
            .with_context(|| format!("无法读取认证 token: {}", token_path.display()))?;
        let authorization = format!("Bearer {}", token.trim())
            .parse()
            .map_err(|error| anyhow::anyhow!("认证 token 不能作为 gRPC 元数据: {error}"))?;
        let endpoint = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint
        } else {
            format!("http://{}", endpoint.trim_start_matches("tcp://"))
        };
        let channel = Channel::from_shared(endpoint.clone())?
            .connect()
            .await
            .with_context(|| format!("无法连接远程 daemon: {endpoint}"))?;
        Ok(Self {
            inner: configured_client(channel),
            authorization: Some(authorization),
        })
    }

    /// 包装消息并附加 TCP 认证元数据。
    fn request<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        if let Some(authorization) = &self.authorization {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
        }
        request
    }
}

/// 客户端包装层公开的非流式生命周期操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 执行 Compose 启动。
    Start,
    /// 执行 Compose 停止。
    Stop,
    /// 执行 Compose 重启。
    Restart,
    /// 执行 Compose 构建。
    Build,
}

/// 配置生成客户端的消息大小限制。
fn configured_client(channel: Channel) -> OrchestratorClient<Channel> {
    OrchestratorClient::new(channel)
        .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
        .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE)
}
