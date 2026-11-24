# megaft — see PLAN.md. Build output, toolchain and registry live on sgoinfre, never in $HOME.

SG               ?= /sgoinfre/$(USER)
export RUSTUP_HOME      ?= $(SG)/.rustup
export CARGO_HOME       ?= $(SG)/.cargo
export CARGO_TARGET_DIR ?= $(SG)/cargo-target
export PATH := $(CARGO_HOME)/bin:$(PATH)

UUID   := ftgnome@zoe.dev.br
EXTDIR := extension/$(UUID)
BINDIR := $(HOME)/.local/bin

.PHONY: bootstrap doctor check fmt clippy test lint-inheritance gui build release install install-gui uninstall ext-sync-xml \
        ext-pack ext-install ext-test server-test clean-cache

## One-time setup: build dir on sgoinfre + ./target symlink for tools that don't read the shell env
bootstrap:
	mkdir -p "$(CARGO_TARGET_DIR)"
	@if [ -e target ] && [ ! -L target ]; then echo "ERROR: ./target is a real directory in \$$HOME; delete it"; exit 1; fi
	ln -sfn "$(CARGO_TARGET_DIR)" target
	@$(MAKE) --no-print-directory doctor

## Fails if anything would put build output or the toolchain in $HOME
doctor:
	@./scripts/doctor.sh

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
	cargo test --workspace --all-features

## Every crate must opt in to the workspace lints (stable cargo doesn't warn about this)
lint-inheritance:
	@./scripts/lint-inheritance.sh

check: doctor lint-inheritance fmt clippy test

build:
	cargo build --workspace

gui:
	cargo run -p evalfinder-gui

release:
	cargo build --workspace --release

## Installs stripped binaries in ~/.local/bin and the systemd/D-Bus units (PLAN §4)
install:
	cargo build --release -p megaftd -p evalfinder-tui
	mkdir -p "$(BINDIR)" "$(HOME)/.config/systemd/user" "$(HOME)/.local/share/dbus-1/services"
	install -m 0755 "$(CARGO_TARGET_DIR)/release/megaftd" "$(CARGO_TARGET_DIR)/release/evalfinder" "$(BINDIR)/"
	install -m 0644 dist-files/megaftd.service "$(HOME)/.config/systemd/user/megaftd.service"
	install -m 0644 dist-files/br.dev.zoe.MegaFt1.service "$(HOME)/.local/share/dbus-1/services/"
	systemctl --user daemon-reload
	systemctl --user enable megaftd.service
	systemctl --user restart megaftd.service

## The GPUI app (separate: its release build is much heavier)
install-gui:
	cargo build --release -p evalfinder-gui
	mkdir -p "$(BINDIR)" "$(HOME)/.local/share/applications"
	install -m 0755 "$(CARGO_TARGET_DIR)/release/evalfinder-gui" "$(BINDIR)/"
	install -m 0644 dist-files/evalfinder.desktop "$(HOME)/.local/share/applications/"

uninstall:
	-systemctl --user disable --now megaftd.service
	rm -f "$(HOME)/.config/systemd/user/megaftd.service" \
	      "$(HOME)/.local/share/dbus-1/services/br.dev.zoe.MegaFt1.service" \
	      "$(HOME)/.local/share/applications/evalfinder.desktop" \
	      "$(BINDIR)/megaftd" "$(BINDIR)/evalfinder" "$(BINDIR)/evalfinder-gui"
	-systemctl --user daemon-reload

## Copy the D-Bus API description into the extension (a test checks they match)
ext-sync-xml:
	cp crates/megaft-proto/megaft1.xml $(EXTDIR)/dbus/megaft1.xml

## GNOME extension (unchanged from ft-gnome)
ext-pack:
	mkdir -p dist
	gnome-extensions pack $(EXTDIR) --extra-source=lib $(if $(wildcard $(EXTDIR)/dbus),--extra-source=dbus) \
	        --out-dir=dist --force

ext-install: ext-pack
	gnome-extensions install --force dist/$(UUID).shell-extension.zip
	@echo "Enable with: gnome-extensions enable $(UUID)"

ext-test:
	(cd server && exec env PORT=3999 bun run scripts/fake-server.ts >/dev/null 2>&1) & pid=$$!; sleep 1; \
	gjs extension/test-client.js http://localhost:3999; rc=$$?; kill $$pid; exit $$rc

server-test:
	cd server && bun test && bunx tsc --noEmit

clean-cache:
	rm -rf "$(CARGO_TARGET_DIR)"
