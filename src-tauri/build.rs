fn main() {
    // whisper's Vulkan backend imports vulkan-1.dll, which only GPU drivers
    // install. Delay-loading it lets the app start without one; gpu.rs loads
    // the bundled copy before ggml first calls into it.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:vulkan-1.dll");
        println!("cargo:rustc-link-lib=delayimp");
    }
    tauri_build::build()
}
