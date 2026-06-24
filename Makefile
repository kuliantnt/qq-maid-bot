CORE_DIR := qq-maid-core
GATEWAY_DIR := qq-maid-gateway-rs
COMMON_DIR := qq-maid-common
BOT_BIN := qq-maid-bot

# status 只统计 Git 已跟踪的 Rust 源码。
# 不统计 target/、脚本、配置、README、Makefile。
STATUS_RUST_PATHS := ':(glob)$(COMMON_DIR)/**/*.rs' ':(glob)$(CORE_DIR)/**/*.rs' ':(glob)$(GATEWAY_DIR)/**/*.rs'

.PHONY: help status build install deploy deploy-local deploy-remote run test test-common test-core test-gateway common-fmt common-test common-check core-fmt core-test core-check gateway-fmt gateway-test gateway-check clean doctor diagnose

help:
	@echo "make status        查看项目状态和 Rust 源码行数"
	@echo "make build         构建统一 qq-maid-bot release 可执行程序"
	@echo "make install       构建 release 二进制并安装到 runtime/ 目录"
	@echo "make deploy-local  构建并部署到本地 runtime/ 目录"
	@echo "make deploy-remote 构建并发布 release 二进制到远端"
	@echo "make run           启动统一 qq-maid-bot 程序"
	@echo "make test          运行根目录 Cargo workspace 的 fmt、test 和 check"
	@echo "make test-common   运行 Rust common fmt check、测试和 check"
	@echo "make test-core     运行 Rust common 和 Core fmt check、测试和 check"
	@echo "make test-gateway  运行 Rust common 和 QQ C2C gateway fmt、测试和 check"
	@echo "make diagnose      运行网络和环境诊断脚本"
	@echo "make clean         清理根目录 Cargo workspace 构建产物"

status:
	@printf '%s\n' '项目状态:'
	@printf '  %-18s %s\n' 'Git 分支' "$$(git branch --show-current 2>/dev/null || printf 'unknown')"
	@printf '  %-18s %s\n' '工作区' "$$(if git diff --quiet --ignore-submodules -- && git diff --cached --quiet --ignore-submodules --; then printf 'clean'; else printf 'dirty'; fi)"
	@printf '  %-18s %s\n' 'Rust 源码文件数' "$$(git ls-files -z -- $(STATUS_RUST_PATHS) | tr '\0' '\n' | sed '/^$$/d' | wc -l | awk '{print $$1}')"
	@printf '  %-18s %s\n' 'Rust 总行数' "$$(git ls-files -z -- $(STATUS_RUST_PATHS) | xargs -0 cat 2>/dev/null | wc -l | awk '{print $$1}')"

run:
	cd runtime && cargo run --manifest-path ../Cargo.toml -p $(BOT_BIN)

doctor: diagnose

diagnose:
	bash scripts/diagnose-network.sh

build:
	cargo build --release --workspace
	@printf 'release 构建完成\n'

# install 将编译产物和控制脚本安装到 runtime/，方便 git clone 后直接使用。
# 安装后进入 runtime/ 目录，按 .env.example 配置 config/.env 即可启动。
install:
	cargo build --release --workspace
	cp -f target/release/$(BOT_BIN) runtime/$(BOT_BIN)
	cp -f scripts/botctl.sh runtime/botctl.sh
	cp -f scripts/diagnose-network.sh runtime/diagnose-network.sh
	cp -f scripts/validate-runtime.sh runtime/validate-runtime.sh
	mkdir -p runtime/static
	find runtime -maxdepth 1 -type f -name 'qq-maid-*' ! -name 'qq-maid-bot' -delete
	find runtime -maxdepth 1 -type f -name '*ctl.sh' ! -name 'botctl.sh' -delete
	chmod +x runtime/$(BOT_BIN) runtime/botctl.sh runtime/diagnose-network.sh runtime/validate-runtime.sh
	@printf '安装完成：runtime/ 目录已包含 release 二进制和控制脚本\n'

deploy: deploy-remote

deploy-local:
	bash scripts/deploy-local.sh

deploy-remote:
	bash scripts/deploy.sh

test:
	cargo fmt --all -- --check
	cargo test --workspace
	cargo check --workspace

test-common: common-fmt common-test common-check

test-core: common-fmt core-fmt common-test core-test common-check core-check

test-gateway: common-fmt gateway-fmt common-test gateway-test common-check gateway-check

common-fmt:
	cargo fmt -p qq-maid-common -- --check

common-test:
	cargo test -p qq-maid-common

common-check:
	cargo check -p qq-maid-common

core-fmt:
	cargo fmt -p qq-maid-core -- --check

core-test:
	cargo test -p qq-maid-core

core-check:
	cargo check -p qq-maid-core

gateway-fmt:
	cargo fmt -p qq-maid-gateway-rs -- --check

gateway-test:
	cargo test -p qq-maid-gateway-rs

gateway-check:
	cargo check -p qq-maid-gateway-rs

clean:
	cargo clean
