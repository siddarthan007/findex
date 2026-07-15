//! Runtime resource policy and human-readable diagnostics.

use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::System;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProfile {
    pub logical_cpus: usize,
    pub rayon_threads: usize,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub process_memory_bytes: u64,
    pub memory_budget_bytes: u64,
    pub cuda_compiled: bool,
    pub gpu_devices: Vec<GpuDevice>,
    pub vector_quantization: String,
    pub recommended_embedding_batch: usize,
    pub onnx_intra_threads: usize,
    pub gpu_memory_limit_bytes: usize,
    pub model_policy: String,
    pub model_profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuDevice {
    pub name: String,
    pub total_memory_mib: u64,
    pub used_memory_mib: u64,
    pub utilization_percent: u8,
    pub temperature_celsius: Option<u8>,
}

pub fn configured_rayon_threads() -> usize {
    let logical = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4);
    std::env::var("FINDEX_RAYON_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| logical.saturating_sub(2).clamp(1, 26))
        .clamp(1, 128)
}

/// Configure the global pool once. Calling this from multiple entry points is
/// harmless: Rayon rejects subsequent initializations and keeps the first.
pub fn configure_runtime() {
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(configured_rayon_threads())
        .thread_name(|index| format!("findex-worker-{index}"))
        .build_global();
}

/// Start one low-frequency janitor for long-running MCP/TUI/desktop hosts.
/// It holds weak references so it cannot keep a server alive by itself.
pub fn start_model_idle_janitor(
    embedder: &Arc<dyn crate::search::vector::Embedder>,
    reranker: &Arc<dyn crate::search::rerank::Reranker>,
) {
    let idle_seconds = std::env::var("FINDEX_MODEL_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300);
    if idle_seconds == 0 {
        return;
    }
    let idle_for = Duration::from_secs(idle_seconds.clamp(30, 86_400));
    let interval = idle_for.div_f32(2.0).max(Duration::from_secs(15));
    let embedder = Arc::downgrade(embedder);
    let reranker = Arc::downgrade(reranker);
    let _ = std::thread::Builder::new()
        .name("findex-model-idle".to_string())
        .spawn(move || loop {
            std::thread::sleep(interval);
            let Some(embedder) = embedder.upgrade() else {
                break;
            };
            let Some(reranker) = reranker.upgrade() else {
                break;
            };
            let embedding_released = embedder.release_idle_resources(idle_for);
            let reranker_released = reranker.release_idle_resources(idle_for);
            if embedding_released || reranker_released {
                eprintln!("Findex released idle ONNX inference sessions");
            }
        });
}

pub fn profile(include_gpu: bool) -> RuntimeProfile {
    let mut system = System::new_all();
    system.refresh_all();
    let total = system.total_memory();
    let available = system.available_memory();
    let process_memory = sysinfo::get_current_pid()
        .ok()
        .and_then(|pid| system.process(pid))
        .map(|process| process.memory())
        .unwrap_or(0);
    let default_budget = (total / 4).min(2 * 1024 * 1024 * 1024);
    let budget = std::env::var("FINDEX_MEMORY_BUDGET_MB")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|mib| mib.saturating_mul(1024 * 1024))
        .unwrap_or(default_budget);
    let gpu_devices = if include_gpu {
        query_nvidia_gpus()
    } else {
        Vec::new()
    };
    let gpu_free_mib = gpu_devices
        .iter()
        .map(|gpu| gpu.total_memory_mib.saturating_sub(gpu.used_memory_mib))
        .max()
        .unwrap_or(0);
    let batch = std::env::var("FINDEX_EMBEDDING_BATCH")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or({
            if gpu_free_mib >= 8192 {
                64
            } else if gpu_free_mib >= 4096 {
                32
            } else if gpu_free_mib > 0 {
                16
            } else if available >= 8 * 1024 * 1024 * 1024 {
                24
            } else {
                8
            }
        });
    RuntimeProfile {
        logical_cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        rayon_threads: configured_rayon_threads(),
        total_memory_bytes: total,
        available_memory_bytes: available,
        process_memory_bytes: process_memory,
        memory_budget_bytes: budget,
        cuda_compiled: cfg!(feature = "cuda"),
        gpu_devices,
        vector_quantization: std::env::var("FINDEX_VECTOR_QUANTIZATION")
            .unwrap_or_else(|_| "bf16".to_string()),
        recommended_embedding_batch: batch.clamp(1, 256),
        onnx_intra_threads: onnx_intra_threads(),
        gpu_memory_limit_bytes: gpu_memory_limit_bytes(),
        model_policy: format!("{:?}", crate::models::model_policy()).to_ascii_lowercase(),
        model_profile: crate::models::model_profile().to_string(),
    }
}

/// Keep ONNX from creating another full-sized CPU pool alongside Rayon.
pub fn onnx_intra_threads() -> usize {
    std::env::var("FINDEX_ONNX_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| configured_rayon_threads().div_ceil(2).clamp(1, 8))
        .clamp(1, 64)
}

/// CUDA arena cap. An explicit environment setting wins; otherwise reserve
/// headroom for the desktop and other agent processes and cap Findex at 4 GiB.
pub fn gpu_memory_limit_bytes() -> usize {
    if let Some(mib) = std::env::var("FINDEX_GPU_MEMORY_LIMIT_MB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        return mib.saturating_mul(1024 * 1024).max(256 * 1024 * 1024);
    }

    let available_mib = query_nvidia_gpus()
        .into_iter()
        .map(|gpu| gpu.total_memory_mib.saturating_sub(gpu.used_memory_mib))
        .max()
        .unwrap_or(2048);
    let budget_mib = available_mib
        .saturating_sub(768)
        .saturating_mul(60)
        .saturating_div(100)
        .clamp(256, 4096);
    (budget_mib as usize).saturating_mul(1024 * 1024)
}

#[cfg(feature = "cuda")]
pub fn cuda_execution_provider() -> ort::ep::CUDA {
    use ort::ep::cuda::ConvAlgorithmSearch;
    use ort::ep::ArenaExtendStrategy;

    let device_id = std::env::var("FINDEX_CUDA_DEVICE_ID")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    ort::ep::CUDA::default()
        .with_device_id(device_id)
        .with_memory_limit(gpu_memory_limit_bytes())
        .with_arena_extend_strategy(ArenaExtendStrategy::SameAsRequested)
        .with_conv_algorithm_search(ConvAlgorithmSearch::Heuristic)
}

fn query_nvidia_gpus() -> Vec<GpuDevice> {
    let mut command = Command::new("nvidia-smi");
    command.args([
        "--query-gpu=name,memory.total,memory.used,utilization.gpu,temperature.gpu",
        "--format=csv,noheader,nounits",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let parts: Vec<_> = line.split(',').map(str::trim).collect();
            if parts.len() < 4 {
                return None;
            }
            Some(GpuDevice {
                name: parts[0].to_string(),
                total_memory_mib: parts[1].parse().ok()?,
                used_memory_mib: parts[2].parse().ok()?,
                utilization_percent: parts[3].parse().unwrap_or(0),
                temperature_celsius: parts.get(4).and_then(|value| value.parse().ok()),
            })
        })
        .collect()
}
