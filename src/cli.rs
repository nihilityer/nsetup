//! Clap 命令界面与客户端请求构造。

/// 命令行参数、子命令与中文帮助。
mod args;
/// 编辑命令到 protobuf 请求的转换。
mod edit;
/// CLI 文件与标准输出操作。
mod io;
/// 镜像拉取进度的终端与管道渲染。
mod progress;
/// 本地及远程命令执行。
mod runner;

pub use args::Cli;
pub use runner::run;
