//! `dgf vscode`: the VS Code extension as a `.vsix` file, which
//! `code --install-extension` installs. The extension's files are compiled
//! into dgf and carry dgf's version, so the two always match.
//!
//! A `.vsix` is a zip archive: the extension under `extension/`, a manifest
//! that names it, and a list of the content types in it.

use std::path::{Path, PathBuf};

/// The extension's files, by their path in `editors/vscode`.
macro_rules! files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../../../editors/vscode/", $path)) as &[u8])),*]
    };
}

const FILES: &[(&str, &[u8])] = files![
    "extension.js",
    "package.json",
    "README.md",
    "lib/copies.js",
    "lib/dashboard.js",
    "lib/defaults.js",
    "lib/dgf.js",
    "lib/duplicates.js",
    "lib/editor.js",
    "lib/model.js",
    "lib/page.js",
    "lib/runs.js",
    "lib/settings.js",
    "lib/side.js",
    "media/dgf.svg",
    "media/icon.png",
];

const VERSION: &str = env!("CARGO_PKG_VERSION");

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension=".js" ContentType="application/javascript"/><Default Extension=".json" ContentType="application/json"/><Default Extension=".md" ContentType="text/markdown"/><Default Extension=".png" ContentType="image/png"/><Default Extension=".svg" ContentType="image/svg+xml"/><Default Extension=".vsixmanifest" ContentType="text/xml"/></Types>
"#;

/// Writes the extension to `output`, or to the temp folder, and says where.
pub fn write(output: Option<PathBuf>) -> Result<PathBuf, String> {
    let path =
        output.unwrap_or_else(|| std::env::temp_dir().join(format!("dog-food-{VERSION}.vsix")));
    std::fs::write(&path, vsix()?)
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    Ok(path)
}

/// The extension as the bytes of a `.vsix` file.
pub fn vsix() -> Result<Vec<u8>, String> {
    let package = package(VERSION)?;
    let mut entries = vec![
        (
            "[Content_Types].xml".to_string(),
            CONTENT_TYPES.as_bytes().to_vec(),
        ),
        (
            "extension.vsixmanifest".to_string(),
            manifest(&package)?.into_bytes(),
        ),
    ];
    for (path, bytes) in FILES {
        let bytes = if *path == "package.json" {
            package.clone().into_bytes()
        } else {
            bytes.to_vec()
        };
        entries.push((format!("extension/{path}"), bytes));
    }
    Ok(zip(&entries))
}

/// The extension's `package.json` with `version` in place of the one in the
/// repository, which no build reads.
fn package(version: &str) -> Result<String, String> {
    const KEY: &str = "\"version\": \"";
    let text = String::from_utf8_lossy(embedded("package.json")).into_owned();
    let start = text.find(KEY).map(|at| at + KEY.len());
    let end = start.and_then(|start| text[start..].find('"').map(|len| start + len));
    match (start, end) {
        (Some(start), Some(end)) => Ok(format!("{}{version}{}", &text[..start], &text[end..])),
        _ => Err("the extension's package.json has no version".to_string()),
    }
}

fn embedded(path: &str) -> &'static [u8] {
    FILES
        .iter()
        .find(|(name, _)| *name == path)
        .map_or(&[], |(_, bytes)| bytes)
}

/// `extension.vsixmanifest`: who the extension is, as its `package.json` says.
fn manifest(package: &str) -> Result<String, String> {
    let json: serde_json::Value = serde_json::from_str(package)
        .map_err(|err| format!("the extension's package.json does not parse: {err}"))?;
    let field = |pointer: &str| xml(json.pointer(pointer).and_then(|v| v.as_str()).unwrap_or(""));
    Ok(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
  <Metadata>
    <Identity Language="en-US" Id="{name}" Version="{version}" Publisher="{publisher}"/>
    <DisplayName>{title}</DisplayName>
    <Description xml:space="preserve">{description}</Description>
    <Properties>
      <Property Id="Microsoft.VisualStudio.Code.Engine" Value="{engine}"/>
      <Property Id="Microsoft.VisualStudio.Code.ExtensionKind" Value="workspace"/>
    </Properties>
  </Metadata>
  <Installation><InstallationTarget Id="Microsoft.VisualStudio.Code"/></Installation>
  <Dependencies/>
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/>
    <Asset Type="Microsoft.VisualStudio.Services.Content.Details" Path="extension/README.md" Addressable="true"/>
    <Asset Type="Microsoft.VisualStudio.Services.Icons.Default" Path="extension/{icon}" Addressable="true"/>
  </Assets>
</PackageManifest>
"#,
        name = field("/name"),
        version = field("/version"),
        publisher = field("/publisher"),
        title = field("/displayName"),
        description = field("/description"),
        engine = field("/engines/vscode"),
        icon = field("/icon"),
    ))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A zip archive with every entry stored as it is: the whole extension is
/// some 50 KB, so compressing it would only add code.
fn zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut files = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in entries {
        let offset = files.len() as u32;
        let described = describe(name, data);
        files.extend(0x0403_4b50u32.to_le_bytes()); // a file follows
        files.extend(&described);
        files.extend(name.as_bytes());
        files.extend(data);
        directory.extend(0x0201_4b50u32.to_le_bytes()); // a directory entry
        directory.extend(20u16.to_le_bytes()); // written by zip 2.0
        directory.extend(&described);
        directory.extend([0u8; 10]); // no comment, first disk, no attributes
        directory.extend(offset.to_le_bytes());
        directory.extend(name.as_bytes());
    }
    let mut archive = files;
    let start = archive.len() as u32;
    archive.extend(&directory);
    archive.extend(0x0605_4b50u32.to_le_bytes()); // the end of the directory
    archive.extend([0u8; 4]); // one disk
    archive.extend((entries.len() as u16).to_le_bytes());
    archive.extend((entries.len() as u16).to_le_bytes());
    archive.extend((directory.len() as u32).to_le_bytes());
    archive.extend(start.to_le_bytes());
    archive.extend(0u16.to_le_bytes()); // no comment
    archive
}

/// What a file's header and its directory entry both say about it.
fn describe(name: &str, data: &[u8]) -> Vec<u8> {
    let size = (data.len() as u32).to_le_bytes();
    let mut out = Vec::new();
    out.extend(20u16.to_le_bytes()); // readable by zip 2.0
    out.extend(0x0800u16.to_le_bytes()); // names are UTF-8
    out.extend(0u16.to_le_bytes()); // stored, not compressed
    out.extend(0u16.to_le_bytes()); // midnight
    out.extend(0x0021u16.to_le_bytes()); // 1 January 1980: the same file from every build
    out.extend(crc32(data).to_le_bytes());
    out.extend(size);
    out.extend(size);
    out.extend((name.len() as u16).to_le_bytes());
    out.extend(0u16.to_le_bytes()); // no extra field
    out
}

/// The checksum zip keeps of every file (CRC-32).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let low = crc & 1;
            crc = (crc >> 1) ^ (0xEDB8_8320 * low);
        }
    }
    !crc
}

/// The command that installs the file, for the user to run.
pub fn install_command(path: &Path) -> String {
    format!("code --install-extension \"{}\" --force", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> usize {
        usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
    }

    fn u32_at(bytes: &[u8], at: usize) -> usize {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    }

    /// Reads an archive back the way an unzip does: from the directory at
    /// its end to each file's own header and bytes.
    fn unzip(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
        let end = archive.len() - 22;
        assert_eq!(
            u32_at(archive, end),
            0x0605_4b50,
            "the end of the directory"
        );
        let mut at = u32_at(archive, end + 16);
        let mut entries = Vec::new();
        for _ in 0..u16_at(archive, end + 10) {
            assert_eq!(u32_at(archive, at), 0x0201_4b50, "a directory entry");
            let (checksum, size) = (u32_at(archive, at + 16), u32_at(archive, at + 24));
            let name_len = u16_at(archive, at + 28);
            let name = String::from_utf8(archive[at + 46..at + 46 + name_len].to_vec()).unwrap();
            let local = u32_at(archive, at + 42);
            assert_eq!(u32_at(archive, local), 0x0403_4b50, "{name}: a file header");
            let data = local + 30 + u16_at(archive, local + 26) + u16_at(archive, local + 28);
            let bytes = archive[data..data + size].to_vec();
            assert_eq!(crc32(&bytes) as usize, checksum, "{name}: its checksum");
            entries.push((name, bytes));
            at += 46 + name_len;
        }
        entries
    }

    #[test]
    fn the_checksum_is_the_one_zip_uses() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn an_archive_gives_back_every_file_it_was_made_of() {
        let entries = vec![
            ("a.txt".to_string(), b"first".to_vec()),
            ("folder/b ü.txt".to_string(), Vec::new()),
            ("c.bin".to_string(), (0..=255).collect()),
        ];
        assert_eq!(unzip(&zip(&entries)), entries);
    }

    #[test]
    fn the_file_holds_the_manifest_and_every_file_of_the_extension() {
        let entries = unzip(&vsix().unwrap());
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            &names[..2],
            ["[Content_Types].xml", "extension.vsixmanifest"]
        );
        assert_eq!(names.len(), FILES.len() + 2);
        for (path, bytes) in FILES.iter().filter(|(path, _)| *path != "package.json") {
            let packed = entries
                .iter()
                .find(|(name, _)| name == &format!("extension/{path}"));
            assert_eq!(
                packed.map(|(_, data)| data.as_slice()),
                Some(*bytes),
                "{path}"
            );
        }
    }

    #[test]
    fn the_extension_carries_the_version_of_the_dgf_that_wrote_it() {
        let entries = unzip(&vsix().unwrap());
        let find = |name: &str| {
            let (_, bytes) = entries.iter().find(|(n, _)| n == name).unwrap();
            String::from_utf8(bytes.clone()).unwrap()
        };
        let package: serde_json::Value =
            serde_json::from_str(&find("extension/package.json")).unwrap();
        assert_eq!(package["version"], VERSION);
        assert_eq!(package["name"], "dog-food");
        assert_eq!(package["main"], "./extension.js");
        let identity = format!(
            r#"<Identity Language="en-US" Id="dog-food" Version="{VERSION}" Publisher="dog-food"/>"#
        );
        assert!(find("extension.vsixmanifest").contains(&identity));
        assert!(find("extension.vsixmanifest").contains(r#"Path="extension/media/icon.png""#));
    }

    #[test]
    fn the_manifest_keeps_markup_in_a_description_as_text() {
        let package = r#"{"name":"n","version":"1.0.0","publisher":"p","displayName":"a & b","description":"<x> \"y\"","engines":{"vscode":"^1.80.0"}}"#;
        let manifest = manifest(package).unwrap();
        assert!(
            manifest.contains("<DisplayName>a &amp; b</DisplayName>"),
            "{manifest}"
        );
        assert!(
            manifest.contains(r#"preserve">&lt;x&gt; &quot;y&quot;</Description>"#),
            "{manifest}"
        );
        assert!(
            manifest.contains(r#"Code.Engine" Value="^1.80.0"/>"#),
            "{manifest}"
        );
    }

    /// A file added to the extension and not to `FILES` would be missing
    /// from every installed copy.
    #[test]
    fn every_file_of_the_extension_is_compiled_in() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../editors/vscode");
        let mut on_disk = Vec::new();
        list(&folder, &folder, &mut on_disk);
        on_disk.sort();
        let mut compiled: Vec<String> = FILES.iter().map(|(path, _)| path.to_string()).collect();
        compiled.sort();
        assert_eq!(compiled, on_disk);
    }

    /// The files under `folder`, without the extension's own tests.
    fn list(root: &Path, folder: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().path())
        {
            let name = entry
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if entry.is_file() {
                found.push(name);
            } else if !["test", "node_modules"].contains(&name.as_str()) {
                list(root, &entry, found);
            }
        }
    }

    #[test]
    fn the_install_command_names_the_file() {
        let command = install_command(Path::new("out/dog-food.vsix"));
        assert_eq!(
            command,
            "code --install-extension \"out/dog-food.vsix\" --force"
        );
    }
}
