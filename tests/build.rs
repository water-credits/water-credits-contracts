//! Build script for the integration-test crate.
//!
//! `credit_factory::register_project()` deploys a real `credit_token` WASM
//! blob via the Soroban deployer, so the full-lifecycle integration test
//! (tests/full_lifecycle.rs) needs the compiled `credit_token.wasm` at test
//! time. Rather than assuming a pre-built artifact in
//! `target/wasm32-unknown-unknown/release/` (which `cargo test -p tests`
//! alone would not produce), this script builds the WASM itself into a
//! private target directory under `OUT_DIR` and exports its path as the
//! `CREDIT_TOKEN_WASM` env var for the tests to read.
//!
//! The nested cargo invocation uses a separate `CARGO_TARGET_DIR` to avoid
//! deadlocking on the workspace target-directory lock held by the outer
//! cargo process.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../contracts/credit_token/src");
    println!("cargo:rerun-if-changed=../contracts/credit_token/Cargo.toml");
    println!("cargo:rerun-if-changed=../contracts/shared/src");
    println!("cargo:rerun-if-changed=../contracts/shared/Cargo.toml");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let wasm_target_dir = out_dir.join("wasm-target");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().to_path_buf();

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(&cargo)
        .current_dir(&workspace_root)
        // Host-build RUSTFLAGS must not leak into the wasm32 build.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("CARGO_TARGET_DIR", &wasm_target_dir)
        .args([
            "build",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "--locked",
            "-p",
            "credit_token",
        ])
        .status()
        .expect("failed to run cargo to build credit_token.wasm");
    if !status.success() {
        panic!(
            "building credit_token.wasm failed. Is the wasm32 target installed? \
             (rustup target add wasm32-unknown-unknown)"
        );
    }

    let wasm_path = wasm_target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("credit_token.wasm");
    assert!(
        wasm_path.exists(),
        "expected wasm artifact at {}",
        wasm_path.display()
    );

    // Normalize the module for the protocol-20 VM — the library equivalent of
    // the Makefile's `fix-wasm` target. rustc ≥ 1.82 encodes call_indirect
    // using the reference-types scheme, which soroban-env-host 20 rejects
    // ("reference-types not enabled: zero byte expected"). Round-tripping
    // through the text format re-encodes the module in MVP form and drops the
    // target_features custom section.
    let wasm_bytes = std::fs::read(&wasm_path).expect("read credit_token.wasm");
    let wat_text = wasmprinter::print_bytes(&wasm_bytes).expect("print credit_token.wasm to wat");
    let fixed = wat::parse_str(&wat_text).expect("re-encode credit_token.wasm from wat");
    let fixed_path = out_dir.join("credit_token.wasm");
    std::fs::write(&fixed_path, fixed).expect("write normalized credit_token.wasm");

    println!("cargo:rustc-env=CREDIT_TOKEN_WASM={}", fixed_path.display());
}
