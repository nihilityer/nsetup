//! gRPC 编排客户端。

use super::proto::orchestrator_client::OrchestratorClient;
use super::proto::{
    CreateApplicationRequest, CreateStaticSiteRequest, DeployStackRequest, GetLogsRequest,
    GetStackRequest, HealthRequest, HealthResponse, InitializeInfrastructureRequest,
    ListStacksRequest, ListStacksResponse, LogLine, OperationResponse, PullProgress,
    RemoveStackRequest, Stack, StackActionRequest,
};
use crate::config::config_dir;
use crate::constants::{AUTH_TOKEN_FILE, GRPC_SOCKET};
use crate::orchestrator::StackAction;
use crate::rpc::MAX_RPC_MESSAGE_SIZE;
use anyhow::Context;
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use tokio::net::UnixStream;
use tonic::Request;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

/// 可连接本机 Unix socket 或远程 TCP 端点的 gRPC 客户端。
#[derive(Debug, Clone)]
pub struct RpcClient {
    /// 底层 Tonic 客户端。
    inner: OrchestratorClient<Channel>,
    /// 远程 TCP 请求使用的认证元数据。
    authorization: Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>,
}

impl RpcClient {
    /// 连接 gRPC 服务。
    ///
    /// 未指定端点时连接 `/run/nsetup/nsetup.sock`。TCP 端点
    /// 必须同时提供令牌文件；Unix socket 的访问控制由文件权限完成。
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

    /// 查询守护进程健康状态。
    pub async fn health(&mut self) -> anyhow::Result<HealthResponse> {
        let request = self.request(HealthRequest {});
        Ok(self.inner.health(request).await?.into_inner())
    }

    /// 初始化 Traefik 基础设施。
    pub async fn initialize_infrastructure(
        &mut self,
        input: InitializeInfrastructureRequest,
    ) -> anyhow::Result<OperationResponse> {
        let request = self.request(input);
        Ok(self
            .inner
            .initialize_infrastructure(request)
            .await
            .map_err(rpc_error)?
            .into_inner())
    }

    /// 生成并部署常规应用。
    pub async fn create_application(
        &mut self,
        input: CreateApplicationRequest,
    ) -> anyhow::Result<OperationResponse> {
        let request = self.request(input);
        Ok(self.inner.create_application(request).await?.into_inner())
    }

    /// 生成并部署静态站点。
    pub async fn create_static_site(
        &mut self,
        input: CreateStaticSiteRequest,
    ) -> anyhow::Result<OperationResponse> {
        let request = self.request(input);
        Ok(self.inner.create_static_site(request).await?.into_inner())
    }

    /// 查询守护进程管理的所有项目。
    pub async fn list_stacks(&mut self) -> anyhow::Result<ListStacksResponse> {
        let request = self.request(ListStacksRequest {});
        Ok(self.inner.list_stacks(request).await?.into_inner())
    }

    /// 查询单个项目。
    pub async fn get_stack(&mut self, name: String) -> anyhow::Result<Stack> {
        let request = self.request(GetStackRequest { name });
        Ok(self.inner.get_stack(request).await?.into_inner())
    }

    /// 创建或更新项目。
    pub async fn deploy(
        &mut self,
        name: String,
        compose_yaml: String,
        env_file: String,
        start: bool,
    ) -> anyhow::Result<OperationResponse> {
        let request = self.request(DeployStackRequest {
            name,
            compose_yaml,
            env_file,
            start,
        });
        Ok(self.inner.deploy_stack(request).await?.into_inner())
    }

    /// 停止并删除项目。
    pub async fn remove(
        &mut self,
        name: String,
        remove_volumes: bool,
    ) -> anyhow::Result<OperationResponse> {
        let request = self.request(RemoveStackRequest {
            name,
            remove_volumes,
        });
        Ok(self.inner.remove_stack(request).await?.into_inner())
    }

    /// 执行项目生命周期操作。
    pub async fn action(
        &mut self,
        name: String,
        action: StackAction,
    ) -> anyhow::Result<OperationResponse> {
        let request = self.request(StackActionRequest { name });
        let response = match action {
            StackAction::Start => self.inner.start_stack(request).await?,
            StackAction::Stop => self.inner.stop_stack(request).await?,
            StackAction::Restart => self.inner.restart_stack(request).await?,
            StackAction::Build => self.inner.build_stack(request).await?,
        };
        Ok(response.into_inner())
    }

    /// 拉取项目镜像并返回服务端进度流。
    pub async fn pull(&mut self, name: String) -> anyhow::Result<tonic::Streaming<PullProgress>> {
        let request = self.request(StackActionRequest { name });
        Ok(self
            .inner
            .pull_stack(request)
            .await
            .map_err(rpc_error)?
            .into_inner())
    }

    /// 获取项目的流式日志。
    pub async fn logs(
        &mut self,
        name: String,
        tail: u32,
        follow: bool,
    ) -> anyhow::Result<tonic::Streaming<LogLine>> {
        let request = self.request(GetLogsRequest { name, tail, follow });
        Ok(self
            .inner
            .get_logs(request)
            .await
            .map_err(rpc_error)?
            .into_inner())
    }

    /// 连接本机 Unix domain socket。
    async fn connect_unix(path: &str) -> anyhow::Result<Self> {
        let socket = PathBuf::from(path);
        let connector_path = socket.clone();
        let channel = Endpoint::try_from("http://[::]:50051")?
            .connect_with_connector(service_fn(move |_| {
                let path = connector_path.clone();
                async move { UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .with_context(|| format!("无法连接本机 gRPC socket: {}", socket.display()))?;
        Ok(Self {
            inner: configured_client(channel),
            authorization: None,
        })
    }

    /// 连接 TCP 端点并加载 Bearer 令牌。
    async fn connect_tcp(endpoint: String, token_file: Option<&Path>) -> anyhow::Result<Self> {
        let token_path = token_file
            .map(Path::to_path_buf)
            .unwrap_or_else(|| config_dir().join(AUTH_TOKEN_FILE));
        let token = std::fs::read_to_string(&token_path)
            .map(|value| value.trim().to_string())
            .with_context(|| format!("无法读取远程认证令牌: {}", token_path.display()))?;
        let authorization = format!("Bearer {token}")
            .parse()
            .map_err(|error| anyhow::anyhow!("认证令牌无法写入 gRPC 元数据: {error}"))?;
        let endpoint = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint
        } else {
            format!("http://{endpoint}")
        };
        let channel = Channel::from_shared(endpoint.clone())?
            .connect()
            .await
            .with_context(|| format!("无法连接远程 gRPC 服务: {endpoint}"))?;
        Ok(Self {
            inner: configured_client(channel),
            authorization: Some(authorization),
        })
    }

    /// 构建请求并在远程 TCP 模式下注入 Bearer 令牌。
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

/// 将协议不匹配转换成可操作的 daemon 升级提示。
fn rpc_error(status: tonic::Status) -> anyhow::Error {
    if status.code() == tonic::Code::Unimplemented {
        anyhow::anyhow!(
            "当前 daemon 不支持该操作，CLI 与 daemon 版本可能不一致；请升级并重启 nsetup.service"
        )
    } else {
        status.into()
    }
}

/// 构造支持静态站点上传大小限制的 Tonic 客户端。
fn configured_client(channel: Channel) -> OrchestratorClient<Channel> {
    OrchestratorClient::new(channel)
        .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE)
        .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
}
