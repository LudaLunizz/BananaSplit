//! Windows resources for `bananasplit.exe`: the app icon (Explorer, the
//! taskbar, shortcuts, Alt+Tab), version information (the file's
//! Properties > Details, the Start Menu) and the application manifest.
//!
//! Written as a `.res` file by hand (the format is simple), so the build
//! needs neither `rc.exe` nor an extra crate, and linked into the program:
//! the MSVC linker takes `.res` files as input.

use std::env;
use std::fs;
use std::path::PathBuf;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;
/// English (United States).
const LANG: u16 = 0x0409;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icons/bananasplit.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let ico = fs::read("assets/icons/bananasplit.ico").expect("assets/icons/bananasplit.ico");
    let res = resources(&ico, &version, &env::var("CARGO_PKG_DESCRIPTION").unwrap_or_default());
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("bananasplit.res");
    fs::write(&out, res).expect("write bananasplit.res");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    } else {
        println!("cargo:warning=no icon or version resource: only the MSVC toolchain is supported");
    }
}

/// The whole `.res` file.
fn resources(ico: &[u8], version: &str, description: &str) -> Vec<u8> {
    let mut res = Vec::new();
    // A .res file starts with an empty entry.
    entry(&mut res, 0, 0, &[]);
    let images = ico_images(ico);
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (i, (dir, data)) in images.iter().enumerate() {
        let id = i as u16 + 1;
        entry(&mut res, RT_ICON, id, data);
        // GRPICONDIRENTRY: the ICONDIRENTRY's first 12 bytes and the id.
        group.extend_from_slice(&dir[..12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut res, RT_GROUP_ICON, 1, &group);
    entry(&mut res, RT_VERSION, 1, &version_info(version, description));
    entry(&mut res, RT_MANIFEST, 1, manifest(version).as_bytes());
    res
}

/// One resource: header (numbered type and name) and data, each padded to
/// four bytes.
fn entry(res: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    let mut h = Vec::new();
    h.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    h.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    h.extend_from_slice(&[0xFF, 0xFF]);
    h.extend_from_slice(&kind.to_le_bytes()); // TYPE
    h.extend_from_slice(&[0xFF, 0xFF]);
    h.extend_from_slice(&id.to_le_bytes()); // NAME
    h.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    // MemoryFlags as rc.exe writes them: MOVEABLE | DISCARDABLE for icon
    // images, | PURE for the group, MOVEABLE | PURE for the rest.
    let flags: u16 = match kind {
        0 => 0,
        RT_ICON => 0x1010,
        RT_GROUP_ICON => 0x1030,
        _ => 0x0030,
    };
    h.extend_from_slice(&flags.to_le_bytes());
    h.extend_from_slice(&(if kind == 0 { 0 } else { LANG }).to_le_bytes()); // LanguageId
    h.extend_from_slice(&0u32.to_le_bytes()); // Version
    h.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    assert_eq!(h.len(), 32);
    res.extend_from_slice(&h);
    res.extend_from_slice(data);
    pad4(res);
}

fn pad4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// The images of an .ico file: each one's 16-byte directory entry and data.
fn ico_images(ico: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);
    assert!(ico.len() >= 6 && u16_at(0) == 0 && u16_at(2) == 1, "not an .ico file");
    (0..usize::from(u16_at(4)))
        .map(|n| {
            let d = 6 + 16 * n;
            let (len, off) = (u32_at(d + 8) as usize, u32_at(d + 12) as usize);
            assert!(off + len <= ico.len(), "truncated .ico file");
            (ico[d..d + 16].to_vec(), ico[off..off + len].to_vec())
        })
        .collect()
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
}

/// A VERSIONINFO block: length, value length, type, key, value, children,
/// each part aligned to four bytes from the start of the resource.
fn block(key: &str, value: &[u8], value_len: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0, 0];
    b.extend_from_slice(&value_len.to_le_bytes());
    b.extend_from_slice(&u16::from(text).to_le_bytes());
    b.extend_from_slice(&utf16z(key));
    pad4(&mut b);
    b.extend_from_slice(value);
    for c in children {
        pad4(&mut b);
        b.extend_from_slice(c);
    }
    let len = b.len() as u16;
    b[..2].copy_from_slice(&len.to_le_bytes());
    b
}

/// "2.7.1" → (2, 7, 1, 0).
fn version_parts(v: &str) -> [u16; 4] {
    let mut p = [0u16; 4];
    for (i, n) in v.split(['.', '-', '+']).take(3).enumerate() {
        p[i] = n.parse().unwrap_or(0);
    }
    p
}

fn version_info(version: &str, description: &str) -> Vec<u8> {
    let [a, b, c, d] = version_parts(version);
    let (ms, ls) = ((u32::from(a) << 16) | u32::from(b), (u32::from(c) << 16) | u32::from(d));
    let mut fixed = Vec::new();
    for v in [
        0xFEEF_04BDu32, // signature
        0x0001_0000,    // structure version
        ms,             // file version
        ls,
        ms, // product version
        ls,
        0x3F,        // flags mask
        0,           // flags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,           // subtype
        0,           // date
        0,
    ] {
        fixed.extend_from_slice(&v.to_le_bytes());
    }
    let file_version = format!("{a}.{b}.{c}.{d}");
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "BananaSplit"),
        ("FileDescription", "BananaSplit"),
        ("FileVersion", file_version.as_str()),
        ("InternalName", "bananasplit"),
        ("LegalCopyright", "MIT License"),
        ("OriginalFilename", "bananasplit.exe"),
        ("ProductName", "BananaSplit"),
        ("ProductVersion", version),
        ("Comments", description),
    ]
    .iter()
    .map(|(k, v)| {
        let value = utf16z(v);
        // For text, the value length counts UTF-16 units, the NUL included.
        block(k, &value, (value.len() / 2) as u16, true, &[])
    })
    .collect();
    let table = block("040904B0", &[], 0, true, &strings);
    let string_info = block("StringFileInfo", &[], 0, true, &[table]);
    let translation = [0x09u8, 0x04, 0xB0, 0x04]; // English (US), Unicode
    let var = block("Translation", &translation, 4, false, &[]);
    let var_info = block("VarFileInfo", &[], 0, true, &[var]);
    block("VS_VERSION_INFO", &fixed, fixed.len() as u16, false, &[string_info, var_info])
}

/// Common Controls 6 (themed dialogs), Windows 10/11 behaviour, per-monitor
/// DPI awareness (sharp on every screen), no elevation, long paths.
fn manifest(version: &str) -> String {
    let [a, b, c, d] = version_parts(version);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="BananaSplit" version="{a}.{b}.{c}.{d}" processorArchitecture="*"/>
  <description>BananaSplit</description>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#
    )
}
