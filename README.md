# Integration Quickstart Documentation Code Snippets

This repository contains runnable code snippets to help integrators get started
with Renegade.

## Rust

### Direct matches

First, set a funded Base Sepolia account's private key as an environment variable:

```bash
export PRIVATE_KEY=0x...
```

Next, run:

```rust
cargo test direct_match_example -- --nocapture
```

### Solver RFQs

You'll first need to contact the Renegade team to get these secret values:

```bash
export EXTERNAL_MATCH_KEY=...
export EXTERNAL_MATCH_SECRET=...
```

Next, run:

```rust
cargo test rfq_example -- --nocapture
```

<!--## Typescript-->

<!--### Direct matches-->

<!--### Solver RFQ-->

