//! Build script for the integration-test crate.
//!
//! Some integration tests deploy real contract WASM through the Soroban test
//! environment. Rather than assuming pre-built artifacts in
//! `target/wasm32-unknown-unknown/release/` (which `cargo test -p tests`
//! alone would not produce), this script builds the required contracts into a
//! private target directory under `OUT_DIR` and exports their normalized paths
//! for the tests to read.
//!
//! The nested cargo invocation uses a separate `CARGO_TARGET_DIR` to avoid
//! deadlocking on the workspace target-directory lock held by the outer
//! cargo process.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const CONTRACTS: [(&str, &str); 2] = [
    ("credit_token", "CREDIT_TOKEN_WASM"),
    ("governance", "GOVERNANCE_WASM"),
];

fn main() {
    println!("cargo:rerun-if-changed=../contracts/credit_token/src");
    println!("cargo:rerun-if-changed=../contracts/credit_token/Cargo.toml");
    println!("cargo:rerun-if-changed=../contracts/governance/src");
    println!("cargo:rerun-if-changed=../contracts/governance/Cargo.toml");
    println!("cargo:rerun-if-changed=../contracts/shared/src");
    println!("cargo:rerun-if-changed=../contracts/shared/Cargo.toml");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let wasm_target_dir = out_dir.join("wasm-target");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().to_path_buf();

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut build = Command::new(&cargo);
    build
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
        ]);
    for (contract, _) in CONTRACTS {
        build.args(["-p", contract]);
    }

    let status = build
        .status()
        .expect("failed to run cargo to build integration-test contract WASM");
    if !status.success() {
        panic!(
            "building integration-test contract WASM failed. Is the wasm32 target installed? \
             (rustup target add wasm32-unknown-unknown)"
        );
    }

    for (contract, env_var) in CONTRACTS {
        normalize_contract(&out_dir, &wasm_target_dir, contract, env_var);
    }
}

fn normalize_contract(out_dir: &Path, wasm_target_dir: &Path, contract: &str, env_var: &str) {
    let wasm_path = wasm_target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(format!("{contract}.wasm"));
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
    let wasm_bytes = std::fs::read(&wasm_path).expect("read contract WASM");
    let wat_text = wasmprinter::print_bytes(&wasm_bytes).expect("print contract WASM to wat");
    let fixed = wat::parse_str(&wat_text).expect("re-encode contract WASM from wat");
    let fixed_path = out_dir.join(format!("{contract}.wasm"));
    std::fs::write(&fixed_path, fixed).expect("write normalized contract WASM");

    println!("cargo:rustc-env={env_var}={}", fixed_path.display());
}
