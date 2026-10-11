//! The shared libraries a binary needs at run time, as the platform's
//! own tool reports them (`ldd` on Linux, `otool -L` on macOS).

use serde::Serialize;
use std::path::Path;
use std::process::Command;

/// What the binary links at run time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Native {
    /// The tool that answered, or `None` when none was available.
    pub tool: Option<&'static str>,
    /// Library names, sorted; empty for a static binary.
    pub libraries: Vec<String>,
}

/// The libraries `binary` needs.
pub(crate) fn libraries(binary: &Path) -> Native {
    let (tool, args): (&'static str, &[&str]) = if cfg!(target_os = "macos") {
        ("otool", &["-L"])
    } else {
        ("ldd", &[])
    };
    let output = Command::new(tool).args(args).arg(binary).output();
    let Ok(output) = output else {
        return Native {
            tool: None,
            libraries: Vec::new(),
        };
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let skip = usize::from(tool == "otool");
    let mut libraries: Vec<String> = text
        .lines()
        .skip(skip)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.starts_with("linux-vdso"))
        .map(library_name)
        .collect();
    libraries.sort();
    libraries.dedup();
    Native {
        tool: Some(tool),
        libraries,
    }
}

/// `/usr/lib/libssl.3.dylib` as `libssl.3.dylib`.
fn library_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}
