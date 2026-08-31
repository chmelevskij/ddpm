# ddpm — DDC/CI monitor control (brightness / contrast / input) for Linux
#
# `just`            list recipes
# `just install`    build + install for the current user (no sudo)
# `just setup-i2c`  one-time system setup so the app can talk to monitors

set shell := ["bash", "-euo", "pipefail", "-c"]

bin := "ddpm"

# User install: where `cargo install` puts binaries (~/.cargo/bin by default)
install_root := env("CARGO_INSTALL_ROOT", env("CARGO_HOME", home_directory() / ".cargo"))
bindir       := install_root / "bin"
appdir       := env("XDG_DATA_HOME", home_directory() / ".local" / "share") / "applications"

# System install (sudo): /usr/local
sys_prefix := "/usr/local"

# List recipes
[private]
default:
    @just --list --unsorted

# ── Development ──────────────────────────────────────────────────────────────

# Type-check without producing a binary
check:
    cargo check --all-targets

# Debug build
build:
    cargo build

# Optimised build → target/release/ddpm
release:
    cargo build --release

# Run the app (debug build); extra args are passed through
run *ARGS:
    cargo run -- {{ARGS}}

# Run with debug logging from the app and the DDC stack
run-debug *ARGS:
    RUST_LOG=warn,ddpm=debug,ddc_hi=debug,ddc_i2c=debug cargo run -- {{ARGS}}

# Run tests
test:
    cargo test

# Format sources
fmt:
    cargo fmt

# Format check + clippy with warnings denied (CI-style)
lint:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings

# Apply clippy's auto-fixes and reformat
fix:
    cargo clippy --fix --allow-dirty --allow-staged --all-targets
    cargo fmt

# fmt + lint + test + release build
ci: lint test release

# Print out-of-date and future-incompatible dependencies
deps-report:
    cargo update --dry-run
    -cargo report future-incompatibilities

# Remove build artifacts
clean:
    cargo clean

# ── Install / uninstall ──────────────────────────────────────────────────────

# Install for the current user: binary → ~/.cargo/bin, launcher → ~/.local/share/applications
install:
    cargo install --path . --locked --target-dir target --root "{{install_root}}"
    install -Dm644 assets/{{bin}}.desktop "{{appdir}}/{{bin}}.desktop"
    sed -i 's|^Exec=.*|Exec={{bindir}}/{{bin}}|' "{{appdir}}/{{bin}}.desktop"
    -update-desktop-database "{{appdir}}" 2>/dev/null
    @echo "Installed {{bindir}}/{{bin}} and {{appdir}}/{{bin}}.desktop"
    @command -v {{bin}} >/dev/null || echo "note: {{bindir}} is not on your PATH"

# Remove the user install
uninstall:
    -cargo uninstall {{bin}} --root "{{install_root}}"
    rm -f "{{appdir}}/{{bin}}.desktop"
    -update-desktop-database "{{appdir}}" 2>/dev/null

# Install system-wide (sudo): binary → /usr/local/bin, launcher → /usr/local/share/applications
install-system: release
    sudo install -Dm755 target/release/{{bin}} "{{sys_prefix}}/bin/{{bin}}"
    sudo install -Dm644 assets/{{bin}}.desktop "{{sys_prefix}}/share/applications/{{bin}}.desktop"
    sudo sed -i 's|^Exec=.*|Exec={{sys_prefix}}/bin/{{bin}}|' "{{sys_prefix}}/share/applications/{{bin}}.desktop"
    -sudo update-desktop-database "{{sys_prefix}}/share/applications" 2>/dev/null

# Remove the system-wide install
uninstall-system:
    sudo rm -f "{{sys_prefix}}/bin/{{bin}}" "{{sys_prefix}}/share/applications/{{bin}}.desktop"
    -sudo update-desktop-database "{{sys_prefix}}/share/applications" 2>/dev/null

# ── System setup (Debian/Ubuntu) ─────────────────────────────────────────────

# Install build dependencies (apt); only libudev is linked at build time, GUI libs are dlopen'ed at runtime
deps:
    sudo apt-get install -y build-essential pkg-config libudev-dev

# One-time setup so a non-root user can talk DDC/CI: i2c-dev module, i2c group, udev rule
setup-i2c:
    #!/usr/bin/env bash
    set -euo pipefail
    # kernel module (a no-op when i2c-dev is built into the kernel)
    if [ ! -e /dev/i2c-0 ] && ! ls /dev/i2c-* >/dev/null 2>&1; then
        sudo modprobe i2c-dev
    fi
    if ! grep -qs '^i2c-dev' /etc/modules-load.d/*.conf /etc/modules 2>/dev/null; then
        echo i2c-dev | sudo tee /etc/modules-load.d/i2c-dev.conf >/dev/null
        echo "wrote /etc/modules-load.d/i2c-dev.conf"
    fi
    # group + udev rule (i2c-tools/ddcutil already ship one; add ours only if none exists)
    getent group i2c >/dev/null || sudo groupadd --system i2c
    if ! grep -qs 'i2c-\[0-9\]' /usr/lib/udev/rules.d/*i2c* /etc/udev/rules.d/*i2c* 2>/dev/null; then
        echo 'SUBSYSTEM=="i2c-dev", KERNEL=="i2c-[0-9]*", GROUP="i2c", MODE="0660"' \
            | sudo tee /etc/udev/rules.d/45-ddcci-i2c.rules >/dev/null
        sudo udevadm control --reload
        sudo udevadm trigger --subsystem-match=i2c-dev
        echo "wrote /etc/udev/rules.d/45-ddcci-i2c.rules"
    fi
    if ! id -nG "$USER" | tr ' ' '\n' | grep -qx i2c; then
        sudo usermod -aG i2c "$USER"
        echo ">> added $USER to group i2c — log out and back in for it to take effect"
    fi
    echo "i2c setup OK"

# Show whether this machine is ready to talk DDC/CI
doctor:
    #!/usr/bin/env bash
    echo "user:      $USER (groups: $(id -nG))"
    echo "i2c-dev:   $( { [ -d /sys/module/i2c_dev ] || grep -qs '^CONFIG_I2C_CHARDEV=y' /boot/config-"$(uname -r)"; } && echo present || echo MISSING )"
    echo "devices:"; ls -l /dev/i2c-* 2>/dev/null | sed 's/^/           /' || echo "           none (run: just setup-i2c)"
    id -nG | tr ' ' '\n' | grep -qx i2c && echo "i2c group: yes" || echo "i2c group: NO (run: just setup-i2c)"
    if command -v ddcutil >/dev/null; then echo "ddcutil:"; ddcutil detect --terse 2>&1 | sed 's/^/           /'; else echo "ddcutil:   not installed (optional: sudo apt install ddcutil)"; fi
    command -v {{bin}} >/dev/null && echo "installed: $(command -v {{bin}})" || echo "installed: no (run: just install)"
