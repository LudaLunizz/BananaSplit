# BananaSplit.

Virtual audio software for Linux and Windows. This is pre-alpha software; treat it as such.

## Downloads

| Platform | Development version | Downloads |
| --- | --- | --- |
| Windows x64 | 0.2.7 | [Portable EXE](0.2.7%20windows%20version/bananasplit.exe) · [Checksums](0.2.7%20windows%20version/bananasplit.exe.sha256) · [Release notes](0.2.7%20windows%20version/README.txt) · [Source](0.2.7%20windows%20version/bananasplit/) |
| Linux x86-64 | 0.2.7 | [Debian/Ubuntu package](0.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit_2.7.0_amd64.deb) · [Portable archive](0.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit-2.7.0-linux-x86_64.tar.gz) · [Source archive](0.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit_2.7.tar.gz) · [Checksums](0.2.7%20channel%20menus%20%2B%20review%20fixes/SHA256SUMS.txt) |

Linux packages require x86-64 and glibc 2.34 or newer. Debian, Ubuntu, Mint, and Pop!_OS users can install the `.deb` with `sudo apt install ./bananasplit_2.7.0_amd64.deb`. Other distributions can extract the portable archive and run `./install.sh`; see [Linux installation instructions](0.2.7%20channel%20menus%20%2B%20review%20fixes/INSTALL.txt).

The Linux desktop app remains active in the system tray when its window is closed. Quit it from the tray icon or the window menu.

The Windows 0.2.7 source targets a native WASAPI desktop app window using Microsoft's WebView2 Runtime. The checked-in portable EXE is still the earlier browser-based build pending a fresh build; see the [Windows release notes](0.2.7%20windows%20version/README.txt).
