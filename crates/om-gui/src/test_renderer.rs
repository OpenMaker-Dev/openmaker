//! Offscreen rendering for unit and integration tests. This module is compiled
//! separately into each test binary, so its device is shared only in that process.

use std::sync::{Arc, OnceLock};

use eframe::egui_wgpu::{self, wgpu};
use egui_kittest::wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer};
use egui_kittest::LazyRenderer;

/// The wgpu device every harness in this test process renders with.
///
/// egui_kittest's default renderer creates a Vulkan instance and device for
/// each harness at its first render and destroys them with the harness. With
/// tests on parallel threads, Vulkan loaders before 1.4.345 race on that
/// (#397): `vkEnumeratePhysicalDevices` on one thread's new instance frees
/// its drivers that have no device under `loader_lock`, while another
/// thread naming its objects (`vkSetDebugUtilsObjectNameEXT` in debug
/// builds) or loading its device (`vkGetDeviceQueue2`) walks every
/// instance's drivers under a different lock, and the test binary dies with
/// SIGSEGV in libvulkan. So the device is created once, the way egui_kittest
/// creates it, and never destroyed.
struct SharedDevice {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    available_adapters: Vec<wgpu::Adapter>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    target_format: wgpu::TextureFormat,
}

fn shared_device() -> &'static SharedDevice {
    static DEVICE: OnceLock<SharedDevice> = OnceLock::new();
    DEVICE.get_or_init(|| {
        let state = create_render_state(
            default_wgpu_setup(),
            egui_wgpu::RendererOptions::PREDICTABLE,
        );
        SharedDevice {
            instance: state.instance,
            adapter: state.adapter,
            available_adapters: state.available_adapters,
            device: state.device,
            queue: state.queue,
            target_format: state.target_format,
        }
    })
}

/// egui_kittest's default renderer on [`shared_device`]: made at the
/// harness's first render, with the harness's own egui renderer and
/// textures. Pass it to every harness that renders.
pub fn renderer() -> LazyRenderer {
    LazyRenderer::new(|| {
        let shared = shared_device();
        let renderer = egui_wgpu::Renderer::new(
            &shared.device,
            shared.target_format,
            egui_wgpu::RendererOptions::PREDICTABLE,
        );
        WgpuTestRenderer::from_render_state(egui_wgpu::RenderState {
            adapter: shared.adapter.clone(),
            available_adapters: shared.available_adapters.clone(),
            instance: shared.instance.clone(),
            device: shared.device.clone(),
            queue: shared.queue.clone(),
            target_format: shared.target_format,
            renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        })
    })
}
