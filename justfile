# herdr task runner
set windows-shell := ["cmd.exe", "/d", "/s", "/c"]

python := if os() == "windows" { "python" } else { "python3" }

# Run tests
test:
    cargo nextest run --locked --no-fail-fast --status-level fail --final-status-level fail --failure-output final --success-output never
    just maintenance-test
    just ui-hot-path-architecture-test
    just integration-assets-test
    just docs-contract-test

# Run repository maintenance contract tests
maintenance-test:
    {{python}} -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_hermes_integration_asset scripts.test_package_windows_conpty scripts.test_unix_installer scripts.test_version_bump_only scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty scripts.test_windows_cross scripts.test_windows_input

# Local interactive Windows Terminal input qualification (never runs in normal CI).
[windows]
test-windows-input *args:
    pwsh -NoProfile -File scripts/test_windows_input.ps1 -AllowInputInjection -ClearClipboard {{args}}

# Run one nextest filter, e.g. `just test-one codex_stale_working`
test-one filter:
    cargo nextest run --locked "{{filter}}" --status-level fail --final-status-level fail --failure-output final --success-output never

# Enforce deterministic UI hot-path architecture boundaries
ui-hot-path-architecture-test:
    {{python}} -m unittest scripts.test_ui_hot_path_architecture

# Run fast local lint checks
[unix]
lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[windows]
lint:
    & .\scripts\windows_check.ps1 -Mode lint

# Run PR CI checks
ci filter='all()': lint
    just ci-tests "{{filter}}"

# Keep the test build independently configurable from clippy in CI.
ci-tests filter='all()':
    cargo nextest run --locked --no-fail-fast -E "{{filter}}" --status-level fail --final-status-level slow --failure-output final --success-output never
    just maintenance-test
    just ui-hot-path-architecture-test
    just integration-assets-test

# Download the Windows SDK once (requires xwin; prompts for Microsoft's SDK license)
[unix]
setup-windows-cross *args:
    {{python}} scripts/windows_cross.py setup {{args}}

# Run Windows target lint from Unix/macOS to catch cfg(windows) compile and clippy failures before CI
[unix]
windows-lint:
    {{python}} scripts/windows_cross.py lint

# Check formatting + run unit tests + Windows target lint + documentation contract tests
[unix]
check: ci windows-lint
    just docs-contract-test
    @echo "docs reminder: if this changes user-facing behavior, make sure the relevant release docs are updated or called out before release."

[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[windows]
check:
    & .\scripts\windows_check.ps1 -Mode check

# Install repo-local git hooks
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/pre-commit
    chmod +x .githooks/commit-msg
    @echo "installed git hooks from .githooks"

# Build release binary
[unix]
build:
    cargo build --release --locked

[windows]
build:
    python scripts/package_windows_conpty.py build-local

# Non-gating full-render scaling profile for background workspaces and active panes
bench-render-scale:
    cargo test --release --locked --bin herdr render_scale_profile -- --ignored --nocapture --test-threads=1

# Profile terminal target name resolution at increasing pane counts.
bench-terminal-targets:
    cargo test --release --locked --bin herdr terminal_target_lookup_profile -- --ignored --nocapture --test-threads=1

# Profile Windows foreground inspection of isolated idle shells, without a server.
[windows]
bench-process-inspection:
    cargo test --release --locked --bin herdr windows_process_inspection_profile -- --ignored --nocapture --test-threads=1

# Profile BSP split collection and construction with balanced and skewed trees.
bench-bsp-layout:
    cargo test --release --locked --bin herdr bsp_layout_profile -- --ignored --nocapture --test-threads=1

# Profile full and retained text, static-image, and unchanged-image updates.
bench-retained-graphics:
    cargo test --release --locked --bin herdr render_scale_profile_retained_graphics -- --ignored --nocapture --test-threads=1

# Profile first-batch latency and aggregate drain cost for external API bursts.
bench-api-fairness:
    cargo test --release --locked --bin herdr external_api_burst_profile -- --ignored --nocapture --test-threads=1

# ~3-5 minute CPU comparison; downloads stable unless HERDR_PERF_BASELINE_BIN is set
bench-release-smoke:
    cargo build --release --locked
    scripts/release_perf_smoke.sh "${CARGO_TARGET_DIR:-target}/release/herdr"

# Test public documentation snapshot and release lifecycle tooling
docs-contract-test:
    bun test ./scripts/docs

# Test bundled agent integration assets
integration-assets-test:
    bun test src/integration/assets/herdr-agent-state.test.ts
    bun test src/integration/assets/opencode/herdr-agent-state.test.ts
    bun test src/integration/assets/opencode/herdr-tui-session.test.ts

# Regenerate the C API bindings with bindgen-cli 0.72.1
libghostty-bindings *clang_args:
    bash scripts/generate_libghostty_bindings.sh {{clang_args}}

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Check that release docs and changelog have been finalized from docs/next before release
release-docs-check:
    python3 scripts/agent_detection_manifest_check.py --require-all-published
    python3 scripts/config_reference_check.py
    node scripts/docs/versions.mjs check
    node scripts/docs/preview.mjs check
    just docs-contract-test
    @test -f docs/next/README.md
    @test -f docs/next/README.ja-JP.md
    @if ! diff -u CHANGELOG.md docs/next/CHANGELOG.md; then \
        echo "error: CHANGELOG.md differs from docs/next/CHANGELOG.md; finalize release notes before releasing"; \
        exit 1; \
    fi
    @for file in CONFIGURATION.md INTEGRATIONS.md SOCKET_API.md; do \
        if [ -e "$file" ]; then \
            echo "error: $file was replaced by technical docs; remove the root copy"; \
            exit 1; \
        fi; \
    done
    @test -d docs/next/website/src/content/docs
    @for file in docs/next/website/src/content/docs/*.mdx; do \
        for locale in ja; do \
            translated="docs/next/website/src/content/docs/$locale/$(basename "$file")"; \
            if [ ! -f "$translated" ]; then \
                echo "error: $translated is missing; translate next docs before releasing"; \
                exit 1; \
            fi; \
        done; \
    done
    @for file in docs/next/website/src/content/docs/ja/*.mdx; do \
        staged="docs/next/website/src/content/docs/$(basename "$file")"; \
        if [ ! -f "$staged" ]; then \
            echo "error: $file has no matching english doc; remove the stale translation"; \
            exit 1; \
        fi; \
    done
    python3 scripts/docs_translation_parity.py --docs-root docs/next/website/src/content/docs

# Validate release docs, render scaling, and end-to-end CPU before release preparation
pre-release-check:
    just release-docs-check
    just bench-render-scale
    just bench-release-smoke
    @echo "release review required: investigate material render-scaling regressions before publishing."
    @echo "release review required: update skills/herdr/SKILL.md for this stable release so it matches the current CLI, IDs, agent lifecycle semantics, and safety guidance."
    @echo "release policy: do not update skills/herdr/SKILL.md between stable releases; preview builds keep the latest stable skill."

# Print default config
default-config:
    cargo run --release --locked -- --default-config
