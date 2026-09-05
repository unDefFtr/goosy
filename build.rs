use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=shaders/rgba_to_nv12.comp");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let output = out.join("rgba_to_nv12.comp.spv");
    println!(
        "cargo:rustc-env=GOOSY_NV12_SHADER_PATH={}",
        output.display()
    );
    let require_shader = env::var_os("GOOSY_REQUIRE_GLSLC").is_some();
    match Command::new("glslc")
        .args(["shaders/rgba_to_nv12.comp", "-o"])
        .arg(&output)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) if require_shader => {
            panic!("glslc failed ({status}); GOOSY_REQUIRE_GLSLC requires the zero-copy shader")
        }
        Ok(status) => {
            println!("cargo:warning=glslc failed ({status}); zero-copy Vulkan mode is unavailable");
        }
        Err(error) if require_shader => {
            panic!("glslc is required to build the Vulkan zero-copy shader: {error}")
        }
        Err(error) => println!(
            "cargo:warning=glslc is required to build the Vulkan zero-copy shader ({error})"
        ),
    }
}
