//! gRPC 协议、客户端与服务端入口。

mod client;
mod conversion;
mod service;
mod transport;

pub use client::RpcClient;
pub use transport::serve;

/// 静态站点上传允许的最大 gRPC 消息大小。
pub const MAX_RPC_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

/// 由 protobuf 生成的 gRPC 类型。
#[allow(
    clippy::missing_docs_in_private_items,
    clippy::doc_markdown,
    clippy::missing_const_for_fn
)]
pub mod proto {
    tonic::include_proto!("nsetup.v1");
}
