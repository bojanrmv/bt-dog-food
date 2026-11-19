//! On Windows, dgf.exe says what it is: a version resource (product, version,
//! who made it) and a manifest. A program file without them is anonymous to
//! Windows and to the security software that decides whether it may run.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // A source build without the resource compiler still gets its dgf.
        if let Err(err) = windows::describe() {
            println!("cargo:warning=dgf.exe is built without its version resource: {err}");
        }
    }
}

#[cfg(windows)]
mod windows {
    const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="dgf" version="VERSION" processorArchitecture="*"/>
  <description>dog-food (dgf) code-health command-line tool</description>
  <!-- Runs with the rights of whoever starts it, never more. -->
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <!-- Windows 10 and 11. -->
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;

    pub fn describe() -> std::io::Result<()> {
        // What Cargo.toml says about the package.
        let package = |name: &str| std::env::var(format!("CARGO_PKG_{name}")).unwrap_or_default();
        // Four numbers, also for a prerelease (0.3.0-rc.1).
        let version = format!(
            "{}.{}.{}.0",
            package("VERSION_MAJOR"),
            package("VERSION_MINOR"),
            package("VERSION_PATCH")
        );
        let author = package("AUTHORS");
        let mut resource = winresource::WindowsResource::new();
        resource
            .set("ProductName", "dog-food (dgf)")
            .set(
                "FileDescription",
                "dog-food (dgf) code-health command-line tool",
            )
            .set("CompanyName", &author)
            .set(
                "LegalCopyright",
                &format!("Copyright (c) {author}. MIT license."),
            )
            .set("InternalName", "dgf")
            .set("OriginalFilename", "dgf.exe")
            .set("Comments", &format!("Source: {}", package("REPOSITORY")))
            .set_manifest(&MANIFEST.replace("VERSION", &version));
        resource.compile()
    }
}
