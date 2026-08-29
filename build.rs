//! 编译并完善 Nihility gRPC 协议生成代码。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/nsetup.proto");
    println!("cargo:rerun-if-changed=proto/stack.proto");
    tonic_prost_build::configure()
        .compile_protos(&["proto/nsetup.proto", "proto/stack.proto"], &["proto"])?;
    Ok(())
}
