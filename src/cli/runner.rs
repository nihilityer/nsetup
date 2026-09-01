//! CLI 本地命令与远程 RPC 命令执行。

use super::args::{Cli, Command};
use super::edit::edit_request;
use super::io::{
    compact_status, read_assets, read_limited, write_diagnostic, write_line, write_new_file,
    write_text,
};
use super::progress::PullProgressRenderer;
use crate::constants::MAX_CONFIG_SIZE;
use crate::install::{InstallOptions, install};
use crate::rpc::proto;
use crate::rpc::{Action, RpcClient};
use crate::template::{TemplateKind, skeleton};
use dialoguer::Confirm;
use tokio_stream::StreamExt;

/// 执行已解析的命令。
///
/// # 错误
///
/// 输入无效、文件操作失败、RPC 失败或 daemon 操作失败时返回错误。
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init(args) => {
            install(InstallOptions {
                domain: args.domain,
                stacks_root: args.stacks_root,
                data_roots: (!args.data_roots.is_empty()).then_some(args.data_roots),
                listen: args.listen,
                docker_socket: args.docker_socket,
                force: args.force,
            })?;
            write_line("nsetup daemon 已安装并启动")?;
            return Ok(());
        }
        Command::Template(args) => {
            let kind = TemplateKind::parse(&args.name)?;
            write_text(skeleton(kind))?;
            return Ok(());
        }
        Command::Daemon => {
            let config = crate::config::Config::load()?;
            return crate::rpc::serve(config).await;
        }
        _ => {}
    }
    let mut client = RpcClient::connect(cli.endpoint.as_deref(), cli.token_file.as_deref()).await?;
    dispatch_remote(&mut client, cli.command).await
}

/// 建立 daemon 连接后分发单个命令。
///
/// # 错误
///
/// 文件、输入、流式传输或 RPC 操作失败时返回错误。
async fn dispatch_remote(client: &mut RpcClient, command: Command) -> anyhow::Result<()> {
    match command {
        Command::Status => {
            let status = client.status().await?;
            write_line(&format!(
                "版本: {}\nDocker: {}\n项目根目录: {}\n主域名: {}",
                status.version,
                if status.docker_available {
                    "可用"
                } else {
                    "不可用"
                },
                status.stacks_root,
                status.domain
            ))?;
        }
        Command::Up(args) => {
            let config_toml = read_limited(&args.file, MAX_CONFIG_SIZE)?;
            let assets = args
                .assets
                .as_deref()
                .map(read_assets)
                .transpose()?
                .unwrap_or_default();
            let stream = client
                .apply(proto::ApplyRequest {
                    config_toml,
                    assets,
                    start: args.start,
                    force: args.force,
                })
                .await?;
            write_operation_stream(stream).await?;
        }
        Command::Import(args) => {
            let compose_yaml = read_limited(&args.file, MAX_CONFIG_SIZE)?;
            let env_file = args
                .env_file
                .as_deref()
                .map(|path| read_limited(path, MAX_CONFIG_SIZE))
                .transpose()?;
            let stream = client
                .import_compose(proto::ImportComposeRequest {
                    name: args.name,
                    compose_yaml,
                    env_file,
                    start: args.start,
                })
                .await?;
            write_operation_stream(stream).await?;
        }
        Command::Export(args) => {
            let response = client.export(args.name).await?;
            if let Some(path) = args.output {
                write_new_file(&path, response.config_toml.as_bytes())?;
            } else {
                write_text(&response.config_toml)?;
            }
        }
        Command::Edit(args) => {
            let stream = client.edit(edit_request(*args)?).await?;
            write_operation_stream(stream).await?;
        }
        Command::List => {
            let response = client.list().await?;
            if response.stacks.is_empty() {
                write_line("没有受管项目")?;
            } else {
                write_line("项目\t服务\t状态")?;
                for stack in response.stacks {
                    write_line(&format!(
                        "{}\t{}\t{}",
                        stack.name,
                        stack.services.join(","),
                        compact_status(&stack.status)
                    ))?;
                }
            }
        }
        Command::Show(args) => {
            let stack = client.get(args.name).await?;
            write_text(&format!(
                "项目名: {}\n服务: {}\n状态: {}\n\n{}",
                stack.name,
                stack.services.join(", "),
                stack.status,
                stack.compose_yaml
            ))?;
        }
        Command::Start(args) => {
            run_action(client, args.name, Action::Start).await?;
        }
        Command::Stop(args) => {
            run_action(client, args.name, Action::Stop).await?;
        }
        Command::Restart(args) => {
            run_action(client, args.name, Action::Restart).await?;
        }
        Command::Build(args) => {
            run_action(client, args.name, Action::Build).await?;
        }
        Command::Pull(args) => run_pull(client, args.name).await?,
        Command::Logs(args) => {
            let mut stream = client.logs(args.name, args.tail, args.follow).await?;
            while let Some(line) = stream.next().await.transpose()? {
                write_line(&line.line)?;
            }
        }
        Command::Remove(args) => run_remove(client, args.name, args.force).await?,
        Command::Init(_) | Command::Template(_) | Command::Daemon => {
            anyhow::bail!("本地命令被错误地发送到 daemon 分发器");
        }
    }
    Ok(())
}

/// 执行一个流式生命周期操作。
async fn run_action(client: &mut RpcClient, name: String, action: Action) -> anyhow::Result<()> {
    let stream = client.action(name, action).await?;
    write_operation_stream(stream).await
}

/// 拉取镜像并选择适合当前 stdout 的进度渲染。
async fn run_pull(client: &mut RpcClient, name: String) -> anyhow::Result<()> {
    let mut stream = client.pull(name).await?;
    let mut renderer = PullProgressRenderer::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(progress) => renderer.render(&progress)?,
            Err(error) => {
                renderer.finish(false)?;
                return Err(error.into());
            }
        }
    }
    renderer.finish(true)
}

/// 完成确认后流式删除项目。
async fn run_remove(client: &mut RpcClient, name: String, force: bool) -> anyhow::Result<()> {
    let confirmed = force
        || Confirm::new()
            .with_prompt(format!("停止并删除项目 {name}？bind mount 数据会保留"))
            .default(false)
            .interact()?;
    if confirmed {
        let stream = client.remove(name).await?;
        write_operation_stream(stream).await
    } else {
        write_line("已取消")
    }
}

/// 显示流式变更阶段，并仅把最终结果写入 stdout。
async fn write_operation_stream(
    mut stream: tonic::Streaming<proto::OperationProgress>,
) -> anyhow::Result<()> {
    let mut completed = false;
    while let Some(progress) = stream.next().await.transpose()? {
        match proto::OperationStage::try_from(progress.stage)? {
            proto::OperationStage::Unspecified => {
                anyhow::bail!("daemon 返回了未指定的操作阶段");
            }
            proto::OperationStage::Queued | proto::OperationStage::Running => {
                write_diagnostic(&progress.message)?;
            }
            proto::OperationStage::Completed => {
                write_line(&progress.message)?;
                completed = true;
            }
        }
    }
    if !completed {
        anyhow::bail!("daemon 未返回操作完成阶段");
    }
    Ok(())
}
