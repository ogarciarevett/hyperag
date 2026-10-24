//! `hyperag models recommend`: one model per slot that fits the resources free right now.
//!
//! Memory estimate per model = parameters x bytes per parameter x 1.2 + its batch working set.
//! The catalog lists models best first per slot; the first that fits the budget is suggested.
//! The suggestion is only a default: every slot stays the user's choice (`hyperag models set`).

use serde::Serialize;

pub struct Candidate {
    pub slot: &'static str,
    pub spec: &'static str,
    pub name: &'static str,
    pub params_millions: f64,
    pub bytes_per_param: f64,
    /// Working set of one batch on top of the weights, in MB.
    pub batch_mb: f64,
    /// `cpu` runs locally through ONNX Runtime; `gpu` needs a CUDA GPU serving it (vLLM).
    pub runs_on: &'static str,
    pub licence: &'static str,
    /// False for a model this build cannot load yet; it is listed but never suggested.
    pub runnable: bool,
}

/// Best first within a slot, placed by the model cards until the public benchmark runs rank them.
/// EmbeddingGemma 2's estimate counts the text graph that is loaded (314 MB at q8), not the
/// multimodal total.
pub const CATALOG: &[Candidate] = &[
    Candidate { slot: "embedder", spec: "http:<vLLM URL>?model=nvidia/Nemotron-3.5-Embed-8B-BF16&dims=1024&truncate=true&normalized=true", name: "Nemotron 3.5 Embed 8B (BF16, first 1024 dims)", params_millions: 8381.0, bytes_per_param: 2.0, batch_mb: 2048.0, runs_on: "gpu", licence: "OpenMDW-1.1", runnable: true },
    Candidate { slot: "embedder", spec: crate::cli::DEFAULT_EMBEDDING, name: "EmbeddingGemma 2 (q8, 768 dims)", params_millions: 314.0, bytes_per_param: 1.0, batch_mb: 512.0, runs_on: "cpu", licence: "Apache-2.0", runnable: true },
    Candidate { slot: "embedder", spec: "hf:Xenova/multilingual-e5-small?revision=761b726dd34fb83930e26aab4e9ac3899aa1fa78&dtype=q8&pooling=mean", name: "multilingual e5 small (q8, 384 dims)", params_millions: 118.0, bytes_per_param: 1.0, batch_mb: 256.0, runs_on: "cpu", licence: "MIT", runnable: true },
    Candidate { slot: "embedder", spec: "none", name: "none (BM25 only)", params_millions: 0.0, bytes_per_param: 0.0, batch_mb: 0.0, runs_on: "cpu", licence: "-", runnable: true },
    Candidate { slot: "reranker", spec: "-", name: "Nemotron 3.5 Rerank 8B (BF16; the Rust build has no HTTP re-ranker yet)", params_millions: 8381.0, bytes_per_param: 2.0, batch_mb: 2048.0, runs_on: "gpu", licence: "OpenMDW-1.1", runnable: false },
    Candidate { slot: "reranker", spec: crate::cli::DEFAULT_RERANKER, name: "bge-reranker-v2-m3 (q8)", params_millions: 568.0, bytes_per_param: 1.0, batch_mb: 512.0, runs_on: "cpu", licence: "Apache-2.0", runnable: true },
    Candidate { slot: "reranker", spec: "hf:Xenova/bge-reranker-base?revision=280bcc27a84e0b898c251e06fddb25171bd9b101&dtype=q8", name: "bge-reranker-base (q8)", params_millions: 278.0, bytes_per_param: 1.0, batch_mb: 256.0, runs_on: "cpu", licence: "MIT", runnable: true },
    Candidate { slot: "reranker", spec: "none", name: "none (fused order)", params_millions: 0.0, bytes_per_param: 0.0, batch_mb: 0.0, runs_on: "cpu", licence: "-", runnable: true },
];

pub fn estimate_mb(c: &Candidate) -> f64 {
    c.params_millions * c.bytes_per_param * 1.2 + c.batch_mb
}

#[derive(Debug, Default, Serialize)]
pub struct Resources {
    pub total_mb: f64,
    pub free_mb: f64,
    pub swap_used_mb: f64,
    pub cores: usize,
    pub apple_silicon: bool,
    /// Free VRAM of the largest CUDA GPU, when `nvidia-smi` answers.
    pub gpu_free_mb: Option<f64>,
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What is free now: macOS from `vm_stat` and `sysctl`, Linux from `/proc/meminfo`, GPUs from `nvidia-smi`.
pub fn detect() -> Resources {
    let mut r = Resources { cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1), ..Default::default() };
    if let Some(meminfo) = std::fs::read_to_string("/proc/meminfo").ok() {
        let kb = |key: &str| meminfo.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()).unwrap_or(0.0);
        r.total_mb = kb("MemTotal:") / 1024.0;
        r.free_mb = kb("MemAvailable:") / 1024.0;
        r.swap_used_mb = (kb("SwapTotal:") - kb("SwapFree:")) / 1024.0;
        // Inside a container /proc/meminfo shows the host; the cgroup limit is what this process gets.
        let cg = |f: &str| std::fs::read_to_string(format!("/sys/fs/cgroup/{f}")).ok().and_then(|v| v.trim().parse::<f64>().ok());
        if let (Some(max), Some(used)) = (cg("memory.max"), cg("memory.current")) {
            r.total_mb = r.total_mb.min(max / 1_048_576.0);
            r.free_mb = r.free_mb.min((max - used) / 1_048_576.0);
        }
    } else if let Some(vm) = run("vm_stat", &[]) {
        let page = vm.split("page size of ").nth(1).and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok()).unwrap_or(16384.0);
        let pages = |key: &str| vm.lines().find(|l| l.starts_with(key)).and_then(|l| l.rsplit(' ').next()?.trim_end_matches('.').parse::<f64>().ok()).unwrap_or(0.0);
        // Free plus memory the system gives back on demand (inactive, speculative, purgeable).
        r.free_mb = (pages("Pages free:") + pages("Pages inactive:") + pages("Pages speculative:") + pages("Pages purgeable:")) * page / 1_048_576.0;
        r.total_mb = run("sysctl", &["-n", "hw.memsize"]).and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0) / 1_048_576.0;
        r.swap_used_mb = run("sysctl", &["-n", "vm.swapusage"])
            .and_then(|s| s.split("used = ").nth(1)?.split('M').next()?.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        r.apple_silicon = run("sysctl", &["-n", "machdep.cpu.brand_string"]).is_some_and(|s| s.starts_with("Apple"));
    }
    r.gpu_free_mb = run("nvidia-smi", &["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
        .and_then(|s| s.lines().filter_map(|l| l.trim().parse::<f64>().ok()).reduce(f64::max));
    r
}

#[derive(Debug, Serialize)]
pub struct Choice {
    pub slot: &'static str,
    pub model: &'static str,
    pub spec: &'static str,
    pub estimate_mb: f64,
    pub runs_on: &'static str,
    pub licence: &'static str,
    pub why: String,
}

/// One choice per slot. The CPU budget is the free memory minus a 2 GB margin, less while the
/// machine is swapping; GPU models need a CUDA GPU with room for them.
pub fn recommend(r: &Resources) -> Vec<Choice> {
    let mut budget = (r.free_mb - 2048.0).max(0.0);
    if r.swap_used_mb > 1024.0 {
        budget *= 0.5;
    }
    let mut gpu = r.gpu_free_mb.unwrap_or(0.0);
    let mut out = vec![];
    for slot in ["embedder", "reranker"] {
        for c in CATALOG.iter().filter(|c| c.slot == slot && c.runnable) {
            let need = estimate_mb(c);
            let (fits, room) = if c.runs_on == "gpu" { (r.gpu_free_mb.is_some() && need <= gpu, gpu) } else { (need <= budget, budget) };
            if !fits {
                continue;
            }
            if c.runs_on == "gpu" {
                gpu -= need;
            } else {
                budget -= need;
            }
            out.push(Choice {
                slot,
                model: c.name,
                spec: c.spec,
                estimate_mb: need,
                runs_on: c.runs_on,
                licence: c.licence,
                why: format!("best listed {slot} that fits: needs about {need:.0} MB of {room:.0} MB free for it"),
            });
            break;
        }
    }
    out.push(Choice {
        slot: "graph",
        model: "Neo4j",
        spec: "HYPERAG_NEO4J_URL",
        estimate_mb: 768.0,
        runs_on: "cpu",
        licence: "GPL-3.0 (Neo4j Community)",
        why: if budget >= 768.0 { "on: links between memories help evidence and handoffs".into() } else { "off: not enough free memory for Neo4j now".into() },
    });
    out.push(Choice { slot: "llm", model: "none", spec: "-", estimate_mb: 0.0, runs_on: "-", licence: "-", why: "Hyperag returns context; the agent's own model answers".into() });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(free_mb: f64, gpu: Option<f64>, swap: f64) -> Resources {
        Resources { total_mb: 36_864.0, free_mb, swap_used_mb: swap, cores: 12, apple_silicon: gpu.is_none(), gpu_free_mb: gpu }
    }

    #[test]
    fn formula() {
        let gemma = CATALOG.iter().find(|c| c.name.starts_with("EmbeddingGemma")).unwrap();
        assert!((estimate_mb(gemma) - (314.0 * 1.0 * 1.2 + 512.0)).abs() < 1e-9);
    }

    #[test]
    fn picks_by_free_memory_per_slot() {
        let big = recommend(&machine(16_000.0, None, 0.0));
        assert_eq!(big[0].model, "EmbeddingGemma 2 (q8, 768 dims)");
        assert_eq!(big[1].model, "bge-reranker-v2-m3 (q8)");
        // The README tier table: these thresholds are what the code does.
        let both = recommend(&machine(4_200.0, None, 0.0));
        assert_eq!((both[0].model, both[1].model), ("EmbeddingGemma 2 (q8, 768 dims)", "bge-reranker-v2-m3 (q8)"));
        let no_rerank = recommend(&machine(3_000.0, None, 0.0));
        assert_eq!(no_rerank[0].model, "EmbeddingGemma 2 (q8, 768 dims)");
        assert!(no_rerank[1].estimate_mb < 1_194.0);
        let small = recommend(&machine(2_600.0, None, 0.0));
        assert!(small[0].model.starts_with("multilingual e5"));
        let tight = recommend(&machine(1_000.0, None, 0.0));
        assert_eq!(tight[0].spec, "none");
        // 8B models only where a GPU has room, and only runnable ones: no GPU re-ranker yet.
        let gpu = recommend(&machine(16_000.0, Some(48_000.0), 0.0));
        assert!(gpu[0].model.starts_with("Nemotron"));
        assert_eq!(gpu[1].model, "bge-reranker-v2-m3 (q8)");
        // Swapping halves the budget.
        let swapping = recommend(&machine(4_000.0, None, 4_000.0));
        assert!(swapping[0].estimate_mb <= (4_000.0 - 2_048.0) / 2.0);
    }
}
