#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
    printf 'This dependency preflight is for Linux hosts.\n' >&2
    exit 1
fi

required_commands=(cargo rustc git c++ pkg-config file ldd readelf python3 perl setsid)
required_modules=(alsa fontconfig openssl vulkan wayland-client xcb-shape xcb-xfixes xkbcommon xkbcommon-x11)
missing=()

for command_name in "${required_commands[@]}"; do
    command -v "$command_name" >/dev/null 2>&1 || missing+=("command:$command_name")
done

if command -v pkg-config >/dev/null 2>&1; then
    for module_name in "${required_modules[@]}"; do
        pkg-config --exists "$module_name" || missing+=("pkg-config:$module_name")
    done
fi

if ((${#missing[@]})); then
    printf 'Echo Linux build dependencies are missing:\n' >&2
    printf '  - %s\n' "${missing[@]}" >&2
    printf '\nArch/CachyOS: sudo pacman -S --needed base-devel pkgconf python perl util-linux alsa-lib fontconfig openssl vulkan-headers vulkan-icd-loader wayland libxcb libxkbcommon libxkbcommon-x11\n' >&2
    printf 'Debian/Ubuntu: sudo apt install build-essential pkg-config python3 perl util-linux libasound2-dev libfontconfig1-dev libssl-dev libvulkan-dev libwayland-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev binutils file\n' >&2
    exit 1
fi

printf 'Echo Linux build dependencies are ready.\n'
