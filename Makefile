LLM_DIR := qq-maid-llm
GATEWAY_DIR := qq-maid-gateway-rs
COMMON_DIR := qq-maid-common

# status 只统计 Git 已跟踪的 Rust 源码。
# 不统计 target/、脚本、配置、README、Makefile。
STATUS_RUST_PATHS := ':(glob)$(COMMON_DIR)/**/*.rs' ':(glob)$(LLM_DIR)/**/*.rs' ':(glob)$(GATEWAY_DIR)/**/*.rs'

.PHONY: help status build build-llm build-gateway install deploy run run-llm run-gateway test test-common test-llm test-gateway common-fmt common-test common-check rust-fmt rust-test rust-check gateway-fmt gateway-test gateway-check clean doctor diagnose

help:
	@echo "make status        查看项目状态和 Rust 源码行数"
	@echo "make build         构建 Rust LLM 和 gateway release 二进制"
	@echo "make build-llm     构建 Rust LLM release 二进制"
	@echo "make build-gateway 构建 Rust QQ C2C gateway release 二进制"
	@echo "make install       构建 release 二进制并安装到 runtime/ 目录"
	@echo "make deploy        构建并发布 release 二进制到远端"
	@echo "make run           启动 Rust QQ C2C gateway"
	@echo "make run-llm       启动 Rust LLM 服务"
	@echo "make run-gateway   启动 Rust QQ C2C gateway"
	@echo "make test          运行根目录 Cargo workspace 的 fmt、test 和 check"
	@echo "make test-common   运行 Rust common fmt check、测试和 check"
	@echo "make test-llm      运行 Rust common 和 LLM fmt check、测试和 check"
	@echo "make test-gateway  运行 Rust common 和 QQ C2C gateway fmt、测试和 check"
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
	cd runtime && cargo run --manifest-path ../Cargo.toml -p qq-maid-llm

run-gateway:
	cd runtime && cargo run --manifest-path ../Cargo.toml -p qq-maid-gateway-rs

build-llm:
	cargo build --release -p qq-maid-llm

build-gateway:
	cargo build --release -p qq-maid-gateway-rs

build:
	cargo build --release --workspace
	@printf 'release 构建完成\n'

# install 将编译产物和控制脚本安装到 runtime/，方便 git clone 后直接使用。
# 安装后进入 runtime/ 目录，按 .env.example 配置 config/.env 即可启动。
install:
	cargo build --release --workspace
	cp -f target/release/qq-maid-llm runtime/qq-maid-llm
	cp -f target/release/qq-maid-gateway-rs runtime/qq-maid-gateway-rs
	cp -f scripts/llmctl.sh runtime/llmctl.sh
	cp -f scripts/gatewayctl.sh runtime/gatewayctl.sh
	cp -f scripts/botctl.sh runtime/botctl.sh
	cp -f scripts/diagnose-network.sh runtime/diagnose-network.sh
	cp -f scripts/validate-runtime.sh runtime/validate-runtime.sh
	mkdir -p runtime/static
	chmod +x runtime/qq-maid-llm runtime/qq-maid-gateway-rs runtime/llmctl.sh runtime/gatewayctl.sh runtime/botctl.sh runtime/diagnose-network.sh runtime/validate-runtime.sh
	@printf '安装完成：runtime/ 目录已包含 release 二进制和控制脚本\n'

deploy:
	bash scripts/deploy.sh

test:
	cargo fmt --all -- --check
	cargo test --workspace
	cargo check --workspace

test-common: common-fmt common-test common-check

test-llm: common-fmt rust-fmt common-test rust-test common-check rust-check

test-gateway: common-fmt gateway-fmt common-test gateway-test common-check gateway-check

common-fmt:
	cargo fmt -p qq-maid-common -- --check

common-test:
	cargo test -p qq-maid-common

common-check:
	cargo check -p qq-maid-common

rust-fmt:
	cargo fmt -p qq-maid-llm -- --check

rust-test:
	cargo test -p qq-maid-llm

rust-check:
	cargo check -p qq-maid-llm

gateway-fmt:
	cargo fmt -p qq-maid-gateway-rs -- --check

gateway-test:
	cargo test -p qq-maid-gateway-rs

gateway-check:
	cargo check -p qq-maid-gateway-rs

clean:
	cargo clean
