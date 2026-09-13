# Oracle. `make`, `make gui`, `sudo make install`. See lazy.toml for the same
# targets under ImLazy; keep the two in step.
#
# `install` puts the `oracle` command, the `raven-oracle` app, its launcher
# entry, metainfo and icon under $(PREFIX), like every other Raven app. It adds
# no service, no autostart entry and nothing under /etc/raven: the app is in
# the launcher, and it runs when you open it.

APP_ID  := com.ravenoracle.Raven
PREFIX  ?= /usr/local
BINDIR  ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
ICONDIR := $(DATADIR)/icons/hicolor/scalable/apps
DESTDIR ?=
CLI     := oracle
GUI     := raven-oracle
TARGET  := target/release

.PHONY: all build build-cli build-minimal test check smoke fmt clean install install-cli uninstall run tui gui snapshot

all: build

build:
	cargo build --release --locked

# The command line and terminal interface without GTK, for a machine with no desktop.
build-cli:
	cargo build --release --locked --no-default-features --features tui --bin $(CLI)

build-minimal:
	cargo build --release --locked --no-default-features

test:
	cargo test --locked --all-targets
	cargo test --locked --no-default-features --all-targets

check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo clippy --locked --no-default-features --all-targets -- -D warnings
	cargo test --locked --all-targets
	cargo test --locked --no-default-features --all-targets

fmt:
	cargo fmt --all

run:
	cargo run --quiet --bin $(CLI) -- doctor

tui:
	cargo run --quiet --bin $(CLI) -- tui

gui:
	cargo run --quiet --bin $(GUI)

# Render every page of the desktop app to a PNG, then quit.
SNAPSHOT_DIR ?= target/snapshots
snapshot:
	rm -rf "$(SNAPSHOT_DIR)"
	RAVEN_ORACLE_SNAPSHOT="$(SNAPSHOT_DIR)" cargo run --quiet --bin $(GUI)
	ls "$(SNAPSHOT_DIR)"

smoke: build
	python3 scripts/tui-smoke.py $(TARGET)/$(CLI)

# Refreshing the caches is what makes the entry show up in launchers; skipped
# under DESTDIR, where the packager runs them itself.
define update-caches
	@if [ -z "$(DESTDIR)" ]; then \
		command -v update-desktop-database >/dev/null 2>&1 && \
			update-desktop-database -q "$(DATADIR)/applications" || true; \
		command -v gtk-update-icon-cache >/dev/null 2>&1 && \
			gtk-update-icon-cache -qtf "$(DATADIR)/icons/hicolor" || true; \
	fi
endef

install: build
	install -Dm755 "$(TARGET)/$(CLI)" "$(DESTDIR)$(BINDIR)/$(CLI)"
	install -Dm755 "$(TARGET)/$(GUI)" "$(DESTDIR)$(BINDIR)/$(GUI)"
	install -Dm644 "data/$(APP_ID).desktop" "$(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop"
	install -Dm644 "data/$(APP_ID).metainfo.xml" "$(DESTDIR)$(DATADIR)/metainfo/$(APP_ID).metainfo.xml"
	install -Dm644 "data/icons/hicolor/scalable/apps/$(APP_ID).svg" "$(DESTDIR)$(ICONDIR)/$(APP_ID).svg"
	$(update-caches)
	@echo ""
	@echo "Installed Raven Oracle to $(PREFIX)."
	@echo "Nothing was enabled, autostarted, or added to /etc/raven."
	@echo "Open it from the launcher, or run: oracle doctor"

# Only the command, for a machine without a desktop.
install-cli: build-cli
	install -Dm755 "$(TARGET)/$(CLI)" "$(DESTDIR)$(BINDIR)/$(CLI)"
	@echo ""
	@echo "Installed $(CLI) to $(BINDIR). Nothing else was installed."

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(CLI)"
	rm -f "$(DESTDIR)$(BINDIR)/$(GUI)"
	rm -f "$(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop"
	rm -f "$(DESTDIR)$(DATADIR)/metainfo/$(APP_ID).metainfo.xml"
	rm -f "$(DESTDIR)$(ICONDIR)/$(APP_ID).svg"
	$(update-caches)
	@echo "Removed Raven Oracle."
	@echo "Run 'oracle forget' first if you also want the config and models gone."

clean:
	cargo clean
