# BananaSplit.

Virtual audio software for Linux and Windows. This is pre-alpha software; treat it as such.

## Downloads

| Platform | Version | Downloads |
| --- | --- | --- |
| Windows x64 | 2.5.0 | [Installer](version%202.5%20Windows/BananaSplit-Setup-v2.5.0-x64.exe) · [Portable EXE](version%202.5%20Windows/bananasplit.exe) · [Checksums](version%202.5%20Windows/SHA256SUMS.txt) |
| Linux x86-64 | 2.6.0 | [Debian/Ubuntu package](version%202.6%20studio%20gui%20%2B%20desktop%20app/bananasplit_2.6.0_amd64.deb) · [Portable archive](version%202.6%20studio%20gui%20%2B%20desktop%20app/bananasplit-2.6.0-linux-x86_64.tar.gz) · [Source archive](version%202.6%20studio%20gui%20%2B%20desktop%20app/bananasplit_2.6.tar.gz) · [Checksums](version%202.6%20studio%20gui%20%2B%20desktop%20app/SHA256SUMS.txt) |

Linux packages require x86-64 and glibc 2.34 or newer. Debian, Ubuntu, Mint, and Pop!_OS users can install the `.deb` with `sudo apt install ./bananasplit_2.6.0_amd64.deb`. Other distributions can extract the portable archive and run `./install.sh`; see [Linux installation instructions](version%202.6%20studio%20gui%20%2B%20desktop%20app/INSTALL.txt).

The Linux desktop app remains active in the system tray when its window is closed. Quit it from the tray icon or the window menu.

Windows users can also get the latest package from the [Build Windows packages](https://github.com/LudaLunizz/BananaSplit/actions/workflows/windows-package.yml) workflow artifact. See [Windows package details](WINDOWS_INSTALLER.txt).
