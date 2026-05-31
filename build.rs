use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=ANDROID_COMPUTER_USE_MCP_SDK_ROOT");
    println!("cargo:rerun-if-env-changed=ANDROID_SDK_ROOT");
    println!("cargo:rerun-if-env-changed=ANDROID_HOME");

    let proto_root = emulator_proto_root();
    let controller_proto = proto_root.join("emulator_controller.proto");

    println!("cargo:rerun-if-changed={}", controller_proto.display());

    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(&[controller_proto], &[proto_root])
        .expect("failed to compile emulator gRPC protos");
}

fn emulator_proto_root() -> PathBuf {
    for key in [
        "ANDROID_COMPUTER_USE_MCP_SDK_ROOT",
        "ANDROID_SDK_ROOT",
        "ANDROID_HOME",
    ] {
        if let Some(root) = env::var_os(key) {
            let root = PathBuf::from(root);
            let candidate = root.join("emulator/lib");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let default = home.join("Android/Sdk/emulator/lib");
    if default.is_dir() {
        return default;
    }

    panic!(
        "unable to locate Android emulator proto directory; set ANDROID_COMPUTER_USE_MCP_SDK_ROOT, ANDROID_SDK_ROOT, or ANDROID_HOME"
    );
}
