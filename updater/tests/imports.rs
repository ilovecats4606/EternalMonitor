//! Static check of the helper executable: it runs from the user's update
//! folder, so it may import only DLLs the Windows loader never takes from the
//! application folder, and must ask for System32-only resolution of the rest.
//! Reads the PE file; runs nothing.
//!
//! `EM_UPDATER_EXE=<path>` checks another build instead of this one.
#![cfg(windows)]

/// KnownDLLs the helper may import, plus core API sets (resolved by the
/// loader's API set schema, never from a folder).
fn allowed(dll: &str) -> bool {
    let dll = dll.to_ascii_lowercase();
    ["kernel32.dll", "ntdll.dll", "oleaut32.dll"].contains(&dll.as_str())
        || dll.starts_with("api-ms-win-core-")
}

const LOAD_LIBRARY_SEARCH_SYSTEM32: u16 = 0x800;

struct Pe<'a> {
    data: &'a [u8],
    /// (virtual address, virtual size, file offset) per section.
    sections: Vec<(u32, u32, u32)>,
    /// (rva, size) per data directory.
    directories: Vec<(u32, u32)>,
}

impl<'a> Pe<'a> {
    fn u16(&self, at: usize) -> Result<u16, String> {
        let bytes = self.data.get(at..at + 2).ok_or("truncated PE")?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&self, at: usize) -> Result<u32, String> {
        let bytes = self.data.get(at..at + 4).ok_or("truncated PE")?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn parse(data: &'a [u8]) -> Result<Self, String> {
        let mut pe = Self {
            data,
            sections: Vec::new(),
            directories: Vec::new(),
        };
        if data.get(..2) != Some(b"MZ") {
            return Err("not an executable".into());
        }
        let header = pe.u32(0x3C)? as usize;
        if data.get(header..header + 4) != Some(b"PE\0\0") {
            return Err("no PE header".into());
        }
        let coff = header + 4;
        let sections = pe.u16(coff + 2)? as usize;
        let optional = coff + 20;
        let optional_size = pe.u16(coff + 16)? as usize;
        if pe.u16(optional)? != 0x20B {
            return Err("not a 64-bit executable".into());
        }
        let count = pe.u32(optional + 108)? as usize;
        for index in 0..count {
            let at = optional + 112 + index * 8;
            pe.directories.push((pe.u32(at)?, pe.u32(at + 4)?));
        }
        let table = optional + optional_size;
        for index in 0..sections {
            let at = table + index * 40;
            pe.sections
                .push((pe.u32(at + 12)?, pe.u32(at + 8)?, pe.u32(at + 20)?));
        }
        Ok(pe)
    }

    fn offset(&self, rva: u32) -> Result<usize, String> {
        self.sections
            .iter()
            .find(|(address, size, _)| rva >= *address && rva < address + size)
            .map(|(address, _, file)| (rva - address + file) as usize)
            .ok_or_else(|| format!("RVA {rva:#x} is outside every section"))
    }

    fn name(&self, rva: u32) -> Result<String, String> {
        let start = self.offset(rva)?;
        let end = self.data[start..]
            .iter()
            .position(|&b| b == 0)
            .ok_or("unterminated name")?;
        Ok(String::from_utf8_lossy(&self.data[start..start + end]).into_owned())
    }

    fn directory(&self, index: usize) -> Option<(u32, u32)> {
        self.directories
            .get(index)
            .copied()
            .filter(|(rva, _)| *rva != 0)
    }

    /// Normal (directory 1) and delay-load (directory 13) imports.
    fn imports(&self) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        if let Some((rva, _)) = self.directory(1) {
            let mut at = self.offset(rva)?;
            loop {
                let lookup = self.u32(at)?;
                let name = self.u32(at + 12)?;
                if lookup == 0 && name == 0 {
                    break;
                }
                names.push(self.name(name)?);
                at += 20;
            }
        }
        if let Some((rva, _)) = self.directory(13) {
            let mut at = self.offset(rva)?;
            loop {
                let name = self.u32(at + 4)?;
                if name == 0 {
                    break;
                }
                names.push(self.name(name)?);
                at += 32;
            }
        }
        Ok(names)
    }

    /// `DependentLoadFlags` from the load-config directory (directory 10).
    fn dependent_load_flags(&self) -> Result<u16, String> {
        let (rva, _) = self.directory(10).ok_or("no load-config directory")?;
        let at = self.offset(rva)?;
        if self.u32(at)? < 80 {
            return Ok(0);
        }
        self.u16(at + 78)
    }
}

/// Every problem with `exe`, so one run shows them all.
fn problems(exe: &[u8]) -> Result<Vec<String>, String> {
    let pe = Pe::parse(exe)?;
    let mut problems: Vec<String> = pe
        .imports()?
        .into_iter()
        .filter(|dll| !allowed(dll))
        .map(|dll| format!("imports {dll}"))
        .collect();
    let flags = pe.dependent_load_flags()?;
    if flags != LOAD_LIBRARY_SEARCH_SYSTEM32 {
        problems.push(format!(
            "DependentLoadFlags is {flags:#06x}, expected 0x0800"
        ));
    }
    Ok(problems)
}

#[test]
fn the_helper_imports_only_system_libraries_resolved_from_system32() {
    let path = std::env::var("EM_UPDATER_EXE")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_eternal-updater").to_string());
    let exe = std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let pe = Pe::parse(&exe).unwrap();
    eprintln!("{path}: imports {:?}", pe.imports().unwrap());
    let found = problems(&exe).unwrap();
    assert!(found.is_empty(), "{path}: {}", found.join("; "));
}

#[test]
fn the_allowlist_is_explicit() {
    for dll in [
        "KERNEL32.dll",
        "ntdll.dll",
        "OLEAUT32.dll",
        "api-ms-win-core-synch-l1-2-0.dll",
    ] {
        assert!(allowed(dll), "{dll}");
    }
    for dll in [
        "VCRUNTIME140.dll",
        "vcruntime140_1.dll",
        "api-ms-win-crt-runtime-l1-1-0.dll",
        "ucrtbase.dll",
        "version.dll",
        "comctl32.dll",
    ] {
        assert!(!allowed(dll), "{dll}");
    }
}
