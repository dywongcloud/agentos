---
key: mem-bc4caef9e5fc9072-628
ns: default
created: 1784354976556
updated: 1784354976556
---

## Resolved mutable: afit-stream-signature

cargo build -p holoiroh-daemon: Finished, warning count 0. cargo build --example executor_probe: Finished, warning count 0. Resolution: native `async fn` in trait works on edition 2024 with #[allow(async_fn_in_trait)] on the trait; observe() returns a concrete `pub type EventStream = Pin<Box<dyn Stream<Item=ExecutorEvent>+Send>>` built via tokio-stream's BroadcastStream + futures_util combinators (filter_map + take_while), no async-stream crate needed. Callers hold the executor by generic bound (E: ComputerUseExecutor) in the probe, avoiding dyn object-safety concerns entirely.
