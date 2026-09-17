CONTRACTS = credit_factory credit_token verification_oracle retirement_registry project_registry governance credit_bridge
WASM_DIR = target/wasm32-unknown-unknown/release

.PHONY: build fix-wasm test lint fmt

build:
	for contract in $(CONTRACTS); do \
		cargo build --target wasm32-unknown-unknown --release -p $$contract; \
	done
	$(MAKE) fix-wasm

fix-wasm:
	for contract in $(CONTRACTS); do \
		wasm-tools print $(WASM_DIR)/$$contract.wasm \
			| wasm-tools parse -o $(WASM_DIR)/$$contract.wasm -; \
		wasm-tools strip -d 'target_features' \
			-o $(WASM_DIR)/$$contract.wasm \
			$(WASM_DIR)/$$contract.wasm; \
	done

test: build
	cargo test --workspace -- --nocapture

lint:
	cargo clippy --workspace -- -D warnings

fmt:
	cargo fmt --check
