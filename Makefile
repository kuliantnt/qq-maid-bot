LLM_DIR := qq-maid-llm
GATEWAY_DIR := qq-maid-gateway-rs

# status 只统计 Git 已跟踪的 Rust 源码。
# 不统计 target/、脚本、配置、README、Makefile。
STATUS_RUST_PATHS := ':(glob)$(LLM_DIR)/**/*.rs' ':(glob)$(GATEWAY_DIR)/**/*.rs'

.PHONY: help status build build-llm build-gateway deploy run run-llm run-gateway test test-llm test-gateway rust-fmt rust-test gateway-fmt gateway-test gateway-check clean doctor diagnose

help:
	@echo "make status        查看项目状态和 Rust 源码行数"
	@echo "make build         构建 Rust LLM 和 gateway release 二进制"
	@echo "make build-llm     构建 Rust LLM release 二进制"
	@echo "make build-gateway 构建 Rust QQ C2C gateway release 二进制"
	@echo "make deploy        构建并发布 release 二进制到远端"
	@echo "make run           启动 Rust QQ C2C gateway"
	@echo "make run-llm       启动 Rust LLM 服务"
	@echo "make run-gateway   启动 Rust QQ C2C gateway"
	@echo "make test          运行根目录 Cargo workspace 的 fmt、test 和 check"
	@echo "make test-llm      运行 Rust LLM fmt check 和测试"
	@echo "make test-gateway  运行 Rust QQ C2C gateway fmt、测试和 check"
	@echo "make diagnose      运行网络和环境诊断脚本"
	@echo "make clean         清理根目录 Cargo workspace 构建产物"

status:
	@printf '%s\n' '项目状态:'
	@printf '  %-18s %s\n' 'Git 分支' "$$(git branch --show-current 2>/dev/null || printf 'unknown')"
	@printf '  %-18s %s\n' '工作区' "$$(if git diff --quiet --ignore-submodules -- && git diff --cached --quiet --ignore-submodules --; then printf 'clean'; else printf 'dirty'; fi)"
	@printf '  %-18s %s\n' 'Rust 源码文件数' "$$(git ls-files -z -- $(STATUS_RUST_PATHS) | tr '\0' '\n' | sed '/^$$/d' | wc -l | awk '{print $$1}')"
	@printf '  %-18s %s\n' 'Rust 总行数' "$$(git ls-files -z -- $(STATUS_RUST_PATHS) | xargs -0 cat 2>/dev/null | wc -l | awk '{print $$1}')"

run: run-gateway

doctor: diagnose

diagnose:
	bash scripts/diagnose-network.sh

run-llm:
	cargo run -p qq-maid-llm

run-gateway:
	cargo run -p qq-maid-gateway-rs

build-llm:
	cargo build --release -p qq-maid-llm

build-gateway:
	cargo build --release -p qq-maid-gateway-rs

build:
	cargo build --release --workspace
	@printf 'release 构建完成\n'

deploy:
	bash scripts/deploy.sh

test:
	cargo fmt --all -- --check
	cargo test --workspace
	cargo check --workspace

test-llm:
	$(MAKE) rust-fmt
	$(MAKE) rust-test

rust-fmt:
	cargo fmt -p qq-maid-llm -- --check

rust-test:
	cargo test -p qq-maid-llm

test-gateway: gateway-fmt gateway-test gateway-check

gateway-fmt:
	cargo fmt -p qq-maid-gateway-rs -- --check

gateway-test:
	cargo test -p qq-maid-gateway-rs

gateway-check:
	cargo check -p qq-maid-gateway-rs

clean:
	cargo clean
