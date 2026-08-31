//! gRPC 协议、服务端实现、传输层与客户端。

/// RPC 客户端与生命周期操作。
mod client;
/// protobuf 与领域类型之间的转换。
mod conversion;
/// gRPC 服务端方法实现。
mod service;
/// Unix socket 与认证 TCP 传输。
mod transport;

/// 从带完整注释的协议定义生成的代码。
#[allow(
    clippy::doc_markdown,
    clippy::missing_const_for_fn,
    clippy::missing_docs_in_private_items
)]
pub mod proto {
    tonic::include_proto!("nsetup.v1");
}

pub use client::{Action, RpcClient};
pub use service::RpcService;
pub use transport::serve;
