# BananaSplit.

Virtual audio software for Linux and Windows. This is pre-alpha software; treat it as such.

## Downloads

| Platform | Development version | Downloads |
| --- | --- | --- |
| Windows x64 | 0.2.5 | [Installer](0.2.5%20Windows/BananaSplit-Setup-v2.5.0-x64.exe) · [Portable EXE](0.2.5%20Windows/bananasplit.exe) · [Checksums](0.2.5%20Windows/SHA256SUMS.txt) |
| Linux x86-64 | 0.2.7 | [Debian/Ubuntu package](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit_2.7.0_amd64.deb) · [Portable archive](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit-2.7.0-linux-x86_64.tar.gz) · [Source archive](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/bananasplit_2.7.tar.gz) · [Checksums](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/SHA256SUMS.txt) |

Linux packages require x86-64 and glibc 2.34 or newer. Debian, Ubuntu, Mint, and Pop!_OS users can install the `.deb` with `sudo apt install ./bananasplit_2.7.0_amd64.deb`. Other distributions can extract the portable archive and run `./install.sh`; see [Linux installation instructions](version%200.2.7%20channel%20menus%20%2B%20review%20fixes/INSTALL.txt).

The Linux desktop app remains active in the system tray when its window is closed. Quit it from the tray icon or the window menu.

Windows users can also get the latest package from the [Build Windows packages](https://github.com/LudaLunizz/BananaSplit/actions/workflows/windows-package.yml) workflow artifact. See [Windows package details](WINDOWS_INSTALLER.txt).
