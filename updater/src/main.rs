// No console window: the host talks to the helper through a pipe.
#![windows_subsystem = "windows"]

fn main() {
    // Before anything else: later DLL loads come from System32 only.
    #[cfg(windows)]
    eternal_updater::win::restrict_dll_search();
    let code = match eternal_updater::HelperArgs::parse(std::env::args_os().skip(1)) {
        Ok(args) => eternal_updater::run(args, &mut std::io::stdout()),
        Err(error) => {
            println!("error: {error}");
            2
        }
    };
    std::process::exit(code);
}
