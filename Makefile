# Simple automation wrapper for the roxy project.
# Usage examples:
#   make build
#   make run CONFIG_PATH=./config.yaml
#   make test

CARGO ?= cargo
CONFIG_PATH ?= config.yaml
DOCKER ?= docker
IMAGE ?= roxy
HELM ?= helm
CHART_DIR ?= etc/helm/roxy

.PHONY: build
build: ## Build release binary
	$(CARGO) build --release

.PHONY: debug
debug: ## Build debug binary
	$(CARGO) build

.PHONY: run
run: ## Run the proxy with CONFIG_PATH (defaults to config.yaml)
	CONFIG_PATH=$(CONFIG_PATH) $(CARGO) run --release

.PHONY: fmt
fmt: ## Format Rust sources
	$(CARGO) fmt

.PHONY: lint
lint: ## Run clippy lint checks
	$(CARGO) clippy --all-targets --all-features -- -D warnings

.PHONY: test
test: ## Execute unit tests
	$(CARGO) test

.PHONY: clean
clean: ## Clean build artifacts
	$(CARGO) clean

.PHONY: docker-build
docker-build: ## Build the Docker image (IMAGE defaults to roxy)
	$(DOCKER) build -t $(IMAGE) -f etc/docker/Dockerfile .

.PHONY: helm-lint
helm-lint: ## Lint the Helm chart
	$(HELM) lint $(CHART_DIR)

.PHONY: helm-package
helm-package: ## Package the Helm chart into a tgz (outputs to ./dist)
	mkdir -p dist
	$(HELM) package $(CHART_DIR) --destination dist
