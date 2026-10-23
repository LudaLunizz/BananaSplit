# BananaSplit.

Virtual audio software for Linux and Windows. This is pre-alpha software; treat it as such.

## Downloads

| Platform | Development version | Downloads |
| --- | --- | --- |
| Windows x64 | 0.2.7 | [Portable EXE](0.2.7%20windows%20version/bananasplit.exe) · [Checksums](0.2.7%20windows%20version/bananasplit.exe.sha256) · [Release notes](0.2.7%20windows%20version/README.txt) · [Source](0.2.7%20windows%20version/bananasplit/) |
| Linux x86-64 | 0.2.7 | [Debian/Ubuntu package](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit_2.7.0_amd64.deb) · [Portable archive](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit-2.7.0-linux-x86_64.tar.gz) · [Source archive](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit_2.7.tar.gz) · [Checksums](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/SHA256SUMS.txt) |

Linux packages require x86-64 and glibc 2.34 or newer. Debian, Ubuntu, Mint, and Pop!_OS users can install the `.deb` with `sudo apt install ./bananasplit_2.7.0_amd64.deb`. Other distributions can extract the portable archive and run `./install.sh`; see [Linux installation instructions](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/INSTALL.txt).

The Linux desktop app remains active in the system tray when its window is closed. Quit it from the tray icon or the window menu.

The Windows 0.2.7 build is a portable WASAPI app with no extra runtime dependencies. Start it with `bananasplit.exe --gui`; see its [Windows release notes](0.2.7%20windows%20version/README.txt) for setup details. The older [Windows installer notes](WINDOWS_INSTALLER.txt) apply to 0.2.5.
