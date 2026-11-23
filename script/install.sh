#!/usr/bin/env sh
set -eu

# Downloads a autocode release from GitHub and unpacks it into ~/.local/.
# Optional: AUTOCODE_VERSION=1.2.3 picks a release, AUTOCODE_BUNDLE_PATH installs a local tarball.
# The ZED_ names of these variables still work for older instructions.

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${AUTOCODE_CHANNEL:-${ZED_CHANNEL:-stable}}"
    repo="${AUTOCODE_RELEASE_REPOSITORY:-}"
    version="${AUTOCODE_VERSION:-${ZED_VERSION:-latest}}"
    bundle_path="${AUTOCODE_BUNDLE_PATH:-${ZED_BUNDLE_PATH:-}}"
    if [ -z "$bundle_path" ] && [ -z "$repo" ]; then
        echo 'Set AUTOCODE_BUNDLE_PATH to a local bundle or AUTOCODE_RELEASE_REPOSITORY to your own owner/repository.' >&2
        exit 1
    fi
    if [ "$version" = "latest" ]; then
        download_base="https://github.com/$repo/releases/latest/download"
    else
        download_base="https://github.com/$repo/releases/download/v$version"
    fi
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/autocode-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/autocode-XXXXXX")"
    fi

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    case "$platform-$arch" in
        macos-arm64* | linux-arm64* | linux-aarch64)
            arch="aarch64"
            ;;
        macos-x86* | linux-x86*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl () {
            command curl -fL "$@"
        }
    elif command -v wget >/dev/null 2>&1; then
        curl () {
            wget -O- "$@"
        }
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi

    "$platform" "$@"

    if [ "$(command -v autocode)" = "$HOME/.local/bin/autocode" ]; then
        echo "autocode has been installed. Run with 'autocode'"
    else
        echo "To run autocode from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run autocode now, '~/.local/bin/autocode'"
    fi
}

linux() {
    if [ -n "$bundle_path" ]; then
        cp "$bundle_path" "$temp/autocode-linux-$arch.tar.gz"
    else
        echo "Downloading autocode version: $version"
        curl "$download_base/autocode-linux-$arch.tar.gz" > "$temp/autocode-linux-$arch.tar.gz"
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="org.autocode.Editor"
        ;;
      dev)
        appid="org.autocode.Editor.Dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="org.autocode.Editor"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/autocode$suffix.app"
    mkdir -p "$HOME/.local/autocode$suffix.app"
    tar -xzf "$temp/autocode-linux-$arch.tar.gz" -C "$HOME/.local/"

    autocode_editor="$HOME/.local/autocode$suffix.app/libexec/autocode-editor"
    if [ -f "$autocode_editor" ] && command -v ldd >/dev/null 2>&1; then
        missing="$(ldd "$autocode_editor" 2>/dev/null | sed -n 's/^[[:space:]]*\(.*\) => not found$/\1/p')"
        if [ -n "$missing" ]; then
            echo "Warning: your system is missing libraries that autocode needs:"
            echo "$missing" | sed 's/^/    /'
            echo "Install them with your package manager, or autocode will fail to start."
        fi
    fi

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    ln -sf "$HOME/.local/autocode$suffix.app/bin/autocode" "$HOME/.local/bin/autocode"

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/autocode$suffix.app/share/applications"
    cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    sed -i "s|Icon=autocode|Icon=$HOME/.local/autocode$suffix.app/share/icons/hicolor/512x512/apps/autocode.png|g" "${desktop_file_path}"
    sed -i "s|Exec=autocode|Exec=$HOME/.local/autocode$suffix.app/bin/autocode|g" "${desktop_file_path}"
}

macos() {
    echo "Downloading autocode version: $version"
    curl "$download_base/autocode-$arch.dmg" > "$temp/autocode-$arch.dmg"
    hdiutil attach -quiet "$temp/autocode-$arch.dmg" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    # Link the binary
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/autocode"
}

main "$@"
