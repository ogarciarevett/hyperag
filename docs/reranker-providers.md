# Re-ranker execution provider

CPU remains the default. `HYPERAG_RERANK_PROVIDER=cpu` explicitly selects the same existing CPU path. Embedding providers and model specifications are unchanged.

On macOS, build with `cargo build --release --locked -p hyperag --features coreml`, then set `HYPERAG_RERANK_PROVIDER=coreml` when starting the service or MCP process. Both fastembed presets and pinned custom ONNX re-rankers receive the provider. Invalid values, unsupported builds/platforms, and CoreML registration failures fail model loading; an explicit request never silently ignores a registration failure.

The optional feature enables the pinned ort crate's CoreML native distribution. It adds download/build cost and Apple framework linking; it stays disabled for ordinary builds. CoreML may assign unsupported model operators to CPU, so registration alone is not proof that a particular model is accelerated. Warm startup, compile cost, quality, CPU/RSS and per-stage latency need a separate gated model trial before changing a live default.

Synthetic selection tests need no model files. On an Apple CoreML build, `cargo test --release --locked -p hyperag --features coreml --lib native_coreml_provider_is_available_and_registers -- --ignored` probes the linked provider and session registration, without model inference or real memories.
