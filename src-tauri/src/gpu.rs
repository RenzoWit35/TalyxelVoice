//! The graphics card whisper and llama-server run on, through Vulkan. Without
//! a usable one both fall back to the CPU.

use std::ffi::CStr;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use whisper_rs_sys as sys;

/// The device both models use.
#[derive(Debug, Clone)]
pub struct Gpu {
    /// Position among ggml's GPUs, which is what whisper's `gpu_device` counts.
    pub index: i32,
    /// e.g. "NVIDIA GeForce RTX 4070 Ti".
    pub description: String,
}

/// Makes sure a Vulkan loader is in the process before ggml first runs. The
/// import of vulkan-1.dll is delay-loaded (see build.rs) because only GPU
/// drivers install it; without one, the bundled copy stands in and simply
/// finds no devices.
#[cfg(windows)]
pub fn load_vulkan(resource_dir: &Path) {
    use windows::core::{w, HSTRING};
    use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LoadLibraryW, LOAD_LIBRARY_SEARCH_SYSTEM32};

    unsafe {
        if LoadLibraryExW(w!("vulkan-1.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).is_ok() {
            return;
        }
        let bundled = resource_dir.join("vulkan").join("vulkan-1.dll");
        match LoadLibraryW(&HSTRING::from(bundled.as_os_str())) {
            Ok(_) => log::info!("no Vulkan driver installed; using the bundled loader"),
            Err(e) => log::error!("couldn't load {}: {e}", bundled.display()),
        }
    }
}

#[cfg(not(windows))]
pub fn load_vulkan(_resource_dir: &Path) {}

/// The dedicated GPU with the most memory, else an integrated one. A desktop
/// with a graphics card often also has the CPU's own GPU, which is far slower.
pub fn pick() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let gpu = find();
        match &gpu {
            Some(gpu) => log::info!("GPU: {}", gpu.description),
            None => log::info!("no GPU found; running on the CPU"),
        }
        gpu
    })
    .as_ref()
}

fn find() -> Option<Gpu> {
    let mut best: Option<(Gpu, (bool, usize))> = None;
    let mut index = 0;
    unsafe {
        for i in 0..sys::ggml_backend_dev_count() {
            let dev = sys::ggml_backend_dev_get(i);
            let kind = sys::ggml_backend_dev_type(dev);
            let dedicated = kind == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU;
            if !dedicated && kind != sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU {
                continue;
            }
            let (mut free, mut total) = (0, 0);
            sys::ggml_backend_dev_memory(dev, &mut free, &mut total);
            let rank = (dedicated, total);
            if best.as_ref().is_none_or(|(_, r)| rank > *r) {
                let description = CStr::from_ptr(sys::ggml_backend_dev_description(dev)).to_string_lossy().into_owned();
                best = Some((Gpu { index, description }, rank));
            }
            index += 1;
        }
    }
    best.map(|(gpu, _)| gpu)
}

/// llama-server's name for the picked GPU ("Vulkan0"), matched by description
/// in its `--list-devices`. `None` leaves the choice to llama-server.
pub fn llama_device(exe: &Path) -> Option<&'static str> {
    static NAME: OnceLock<Option<String>> = OnceLock::new();
    NAME.get_or_init(|| {
        let gpu = pick()?;
        let mut cmd = Command::new(exe);
        cmd.arg("--list-devices");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = cmd.output().map_err(|e| log::warn!("llama-server --list-devices: {e}")).ok()?;
        let listing = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        let wanted = format!("{} (", gpu.description);
        let name = listing.lines().find_map(|line| {
            let (name, rest) = line.trim().split_once(": ")?;
            rest.starts_with(&wanted).then(|| name.to_owned())
        });
        if name.is_none() {
            log::warn!("llama-server doesn't list {}; it picks its own devices", gpu.description);
        }
        name
    })
    .as_deref()
}
