#!/bin/sh
# BananaSplit, portable package: installs it for you (no root needed).
#
#   ./install.sh               copy bananasplit to ~/.local/bin and add it, with
#                              its icon, to the app menu (run it again to update)
#   ./install.sh --uninstall   remove that again (settings and recordings stay)
#
# Nothing outside your home folder is touched. It also tells you which system
# packages BananaSplit still needs, if any.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=$HERE/bananasplit

case "${1:-}" in
  "") ;;
  --uninstall)
    # The installed copy knows what it installed; this one works as well.
    if [ -x "$HOME/.local/bin/bananasplit" ] && "$HOME/.local/bin/bananasplit" --version >/dev/null 2>&1; then
      exec "$HOME/.local/bin/bananasplit" --uninstall
    fi
    exec "$BIN" --uninstall ;;
  -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) echo "usage: $0 [--uninstall]" >&2; exit 2 ;;
esac

# Does it run here at all? (It needs glibc 2.34 or newer and an x86-64 CPU.)
if ! err=$("$BIN" --version 2>&1 >/dev/null); then
  echo "BananaSplit cannot run on this system:" >&2
  echo "  $err" >&2
  if echo "$err" | grep -q GLIBC; then
    echo "It needs glibc 2.34 or newer (Ubuntu 22.04, Debian 12, Fedora 35, RHEL 9 or later);" >&2
    echo "this system has: $(ldd --version 2>/dev/null | head -n 1)" >&2
  fi
  exit 1
fi

"$BIN" --install || exit 1

# ---- what the system still needs (information only) ----
LIBDIRS="/usr/lib/x86_64-linux-gnu /lib/x86_64-linux-gnu /usr/lib64 /lib64 /usr/lib /usr/local/lib"
LDCACHE=$( (ldconfig -p || /sbin/ldconfig -p || /usr/sbin/ldconfig -p) 2>/dev/null)
have_lib() {
  for name in "$@"; do
    for d in $LIBDIRS; do [ -e "$d/$name" ] && return 0; done
    case "$LDCACHE" in *"$name "*) return 0 ;; esac
  done
  return 1
}
have_pw_jack() {
  for d in $LIBDIRS; do [ -e "$d/pipewire-0.3/jack/libjack.so.0" ] && return 0; done
  return 1
}

os_field() { sed -n "s/^$1=//p" /etc/os-release 2>/dev/null | head -n 1 | tr -d "\"'"; }
ID=$(os_field ID); ID_LIKE=$(os_field ID_LIKE)
family=other
case " $ID $ID_LIKE " in
  *" debian "*|*" ubuntu "*) family=debian ;;
  *" fedora "*|*" rhel "*|*" centos "*) family=fedora ;;
  *" arch "*) family=arch ;;
  *" suse "*|*" opensuse "*) family=suse ;;
esac
case $family in
  debian) inst="sudo apt install"; P_JACK=pipewire-jack; P_PACTL=pulseaudio-utils; P_WEB=libwebkit2gtk-4.1-0 ;;
  fedora) inst="sudo dnf install"; P_JACK=pipewire-jack-audio-connection-kit; P_PACTL=pulseaudio-utils; P_WEB=webkit2gtk4.1 ;;
  arch)   inst="sudo pacman -S"; P_JACK=pipewire-jack; P_PACTL=libpulse; P_WEB=webkit2gtk-4.1 ;;
  suse)   inst="sudo zypper install"; P_JACK=pipewire-libjack-0_3; P_PACTL=pulseaudio-utils; P_WEB=libwebkit2gtk-4_1-0 ;;
  *)      inst=""; P_JACK="PipeWire's JACK library (or JACK2)"; P_PACTL="pactl (PulseAudio utilities)"; P_WEB="WebKitGTK 4.1" ;;
esac

need=""; why=""
if ! have_lib libjack.so.0 && ! have_pw_jack; then
  need="$need $P_JACK"; why="${why}
  - $P_JACK: required, BananaSplit is a JACK client (PipeWire runs JACK clients)"
fi
if ! command -v pactl >/dev/null 2>&1; then
  need="$need $P_PACTL"; why="${why}
  - $P_PACTL: for the default devices, the virtual cables and the apps in the channel menus"
fi
if ! have_lib libwebkit2gtk-4.1.so.0 libwebkit2gtk-4.0.so.37; then
  need="$need $P_WEB"; why="${why}
  - $P_WEB: for BananaSplit's own window (without it, it opens in a browser)"
fi

echo
if [ -n "$need" ]; then
  echo "BananaSplit still needs:$why"
  [ -n "$inst" ] && echo "Install them with:  $inst$need"
  echo
fi
echo "Start BananaSplit from your app menu, or with:  $HOME/.local/bin/bananasplit --gui"
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) echo "(~/.local/bin is not on your PATH, so in a terminal use the full path above.)" ;;
esac
