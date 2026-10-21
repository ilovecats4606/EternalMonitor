//! Chooses the graphics adapter for the window. wgpu takes the first adapter
//! of the preferred GPU type, which is Vulkan on Windows. Old Intel drivers
//! (UHD 620 with 23.20.16.x, issue #59) offer a Vulkan adapter that lacks
//! WebGPU features, and the process dies inside the driver while creating the
//! device, with no error to catch. Prefer a fully capable adapter of the same
//! GPU type, usually DX12 there. A marker file names the backend while the
//! window starts; if a launch dies before painting, the next launch avoids it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
use eframe::wgpu::{self, Backend, DeviceType};
use tracing::{info, warn};

const STARTING_FILE: &str = "renderer-starting";
const AVOID_FILE: &str = "renderer-avoid";

static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();
static FRAMES: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Candidate {
    backend: Backend,
    device_type: DeviceType,
    compliant: bool,
}

/// Install the adapter selector, after recording a failed previous start.
pub(super) fn configure(config: &mut WgpuConfiguration) {
    let directory = crate::settings::app_data_dir();
    let avoid = directory
        .as_deref()
        .map(previous_failures)
        .unwrap_or_default();
    if let Some(directory) = directory {
        let _ = STATE_DIR.set(directory);
    }
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
            select(adapters, surface, &avoid)
        }));
    }
}

/// Called after each frame. The second frame means the first one was
/// presented, so the renderer works on this PC.
pub(super) fn frame_painted() {
    if FRAMES.fetch_add(1, Ordering::Relaxed) != 1 {
        return;
    }
    if let Some(directory) = STATE_DIR.get() {
        let _ = fs::remove_file(directory.join(STARTING_FILE));
    }
    crate::startup::mark_window_shown();
    info!("Window renderer is drawing");
}

fn select(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    avoid: &[String],
) -> Result<wgpu::Adapter, String> {
    let usable: Vec<&wgpu::Adapter> = adapters
        .iter()
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
        .collect();
    let candidates: Vec<Candidate> = usable
        .iter()
        .map(|adapter| {
            let info = adapter.get_info();
            Candidate {
                backend: info.backend,
                device_type: info.device_type,
                compliant: adapter.get_downlevel_capabilities().is_webgpu_compliant(),
            }
        })
        .collect();
    let index = pick(&candidates, avoid)
        .ok_or_else(|| "No graphics adapter can draw the EternalMonitor window".to_string())?;
    let adapter = usable[index].clone();
    let info = adapter.get_info();
    info!(
        backend = %info.backend,
        adapter = %info.name,
        driver = %info.driver_info,
        compliant = candidates[index].compliant,
        avoided = ?avoid,
        "Window renderer selected"
    );
    if let Some(directory) = STATE_DIR.get() {
        let _ = fs::write(directory.join(STARTING_FILE), info.backend.to_str());
    }
    crate::startup::simulate_renderer_failure()?;
    Ok(adapter)
}

/// The best candidate: not avoided, then wgpu's high-performance GPU type order,
/// then fully capable, then wgpu's own backend order.
fn pick(candidates: &[Candidate], avoid: &[String]) -> Option<usize> {
    let type_rank = |device_type| match device_type {
        DeviceType::DiscreteGpu => 0,
        DeviceType::IntegratedGpu => 1,
        DeviceType::Other => 2,
        DeviceType::VirtualGpu => 3,
        DeviceType::Cpu => 4,
    };
    let backend_rank = |backend| match backend {
        Backend::Vulkan | Backend::Metal => 0,
        Backend::Dx12 => 1,
        Backend::Gl => 2,
        _ => 3,
    };
    candidates
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| {
            (
                avoid.iter().any(|name| name == c.backend.to_str()),
                type_rank(c.device_type),
                !c.compliant,
                backend_rank(c.backend),
            )
        })
        .map(|(index, _)| index)
}

/// Backends to avoid. A marker left by the previous launch means it ended
/// before its window painted, so its backend joins the list for good.
fn previous_failures(directory: &Path) -> Vec<String> {
    let avoid_path = directory.join(AVOID_FILE);
    let mut avoid: Vec<String> = fs::read_to_string(&avoid_path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let starting = directory.join(STARTING_FILE);
    if let Ok(backend) = fs::read_to_string(&starting) {
        let backend = backend.trim().to_string();
        warn!(
            backend = %backend,
            "The previous launch closed before its window appeared; using another renderer"
        );
        if !backend.is_empty() && !avoid.contains(&backend) {
            avoid.push(backend);
            if let Err(error) = fs::write(&avoid_path, avoid.join("\n")) {
                warn!(%error, "Could not save the renderer to avoid");
            }
        }
        let _ = fs::remove_file(starting);
    }
    avoid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(backend: Backend, device_type: DeviceType, compliant: bool) -> Candidate {
        Candidate {
            backend,
            device_type,
            compliant,
        }
    }

    /// The adapters wgpu listed on the UHD 620 host from issue #59.
    fn uhd_620() -> Vec<Candidate> {
        vec![
            candidate(Backend::Vulkan, DeviceType::IntegratedGpu, false),
            candidate(Backend::Dx12, DeviceType::IntegratedGpu, true),
            candidate(Backend::Dx12, DeviceType::Cpu, true),
            candidate(Backend::Gl, DeviceType::IntegratedGpu, false),
        ]
    }

    #[test]
    fn an_incomplete_vulkan_driver_yields_to_dx12_on_the_same_gpu() {
        assert_eq!(pick(&uhd_620(), &[]), Some(1));
    }

    #[test]
    fn capable_vulkan_stays_the_default() {
        let candidates = [
            candidate(Backend::Vulkan, DeviceType::IntegratedGpu, true),
            candidate(Backend::Dx12, DeviceType::IntegratedGpu, true),
        ];
        assert_eq!(pick(&candidates, &[]), Some(0));
    }

    #[test]
    fn gpu_type_outranks_capability() {
        let candidates = [
            candidate(Backend::Dx12, DeviceType::IntegratedGpu, true),
            candidate(Backend::Vulkan, DeviceType::DiscreteGpu, false),
        ];
        assert_eq!(pick(&candidates, &[]), Some(1));
    }

    #[test]
    fn a_backend_that_failed_before_is_tried_last() {
        assert_eq!(pick(&uhd_620(), &["dx12".to_string()]), Some(0));
        let avoid = ["vulkan".to_string(), "dx12".to_string()];
        assert_eq!(pick(&uhd_620(), &avoid), Some(3));
        let avoid = ["dx12".to_string(), "gl".to_string(), "vulkan".to_string()];
        // With everything avoided, the usual order applies again.
        assert_eq!(pick(&uhd_620(), &avoid), Some(1));
        assert_eq!(pick(&[], &[]), None);
    }

    #[test]
    fn a_launch_that_never_painted_adds_its_backend_once() {
        let directory = std::env::temp_dir().join(format!(
            "eternal-renderer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        assert!(previous_failures(&directory).is_empty());

        fs::write(directory.join(STARTING_FILE), "vulkan").unwrap();
        assert_eq!(previous_failures(&directory), ["vulkan"]);
        assert!(!directory.join(STARTING_FILE).exists());
        // Remembered after a successful launch, without growing.
        assert_eq!(previous_failures(&directory), ["vulkan"]);
        fs::write(directory.join(STARTING_FILE), "vulkan").unwrap();
        assert_eq!(previous_failures(&directory), ["vulkan"]);
        fs::write(directory.join(STARTING_FILE), "dx12").unwrap();
        assert_eq!(previous_failures(&directory), ["vulkan", "dx12"]);
        fs::remove_dir_all(directory).unwrap();
    }
}
