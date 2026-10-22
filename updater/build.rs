//! The helper runs from the user's update folder, where anyone running as the
//! user can drop a DLL. Link the C runtime statically so no VCRUNTIME140.dll or
//! ucrtbase.dll is imported, and tell the loader to resolve the remaining
//! imports from System32 only (DependentLoadFlags = LOAD_LIBRARY_SEARCH_SYSTEM32).
//! Only the helper's own executable is affected; the host keeps its CRT.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !msvc {
        return;
    }
    for arg in [
        // Replace the DLL CRT import libraries with their static versions.
        "/NODEFAULTLIB:msvcrt.lib",
        "/NODEFAULTLIB:msvcrtd.lib",
        "/NODEFAULTLIB:vcruntime.lib",
        "/NODEFAULTLIB:vcruntimed.lib",
        "/NODEFAULTLIB:ucrt.lib",
        "/NODEFAULTLIB:ucrtd.lib",
        "/DEFAULTLIB:libcmt.lib",
        "/DEFAULTLIB:libvcruntime.lib",
        "/DEFAULTLIB:libucrt.lib",
        // 0x800 = LOAD_LIBRARY_SEARCH_SYSTEM32 for every static import.
        "/DEPENDENTLOADFLAG:0x800",
    ] {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}
