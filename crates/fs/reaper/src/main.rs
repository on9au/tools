//! reaper — find and kill the processes that hold a lock on a file or directory.
//!
//! Windows has no file-lock CLI equivalent of `fuser`/`lsof` + `kill`, so this
//! tool leans on the Restart Manager API (`rstrtmgr.dll`) to discover which
//! processes have a given path open, then terminates them at a chosen severity:
//!
//! * `graceful` — ask the holders to shut down cleanly (≈ `SIGTERM`).
//! * `forced` — let the Restart Manager force-terminate stubborn holders
//!   (≈ `SIGTERM`, escalating to `SIGKILL`).
//! * `kill` — immediately `TerminateProcess` every holder, bypassing the
//!   Restart Manager entirely (≈ `SIGKILL`).

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// Find and kill the processes holding a lock on a file or directory.
#[derive(Debug, Parser)]
#[command(name = "reaper", version, about, long_about = None)]
struct Cli {
    /// The file or directory whose lock holders should be reaped.
    ///
    /// For a directory, every file beneath it is registered with the Restart
    /// Manager, so any process holding any contained file is reported.
    path: PathBuf,

    /// How aggressively to terminate the processes holding the lock.
    #[arg(value_enum, short, long, default_value_t = Severity::Graceful)]
    severity: Severity,

    /// Only list the processes holding the lock; do not terminate anything.
    #[arg(short, long)]
    list: bool,

    /// Skip the confirmation prompt and reap immediately.
    #[arg(short = 'y', long)]
    yes: bool,

    /// Exit code reported by force-killed processes (`kill` severity only).
    #[arg(long, default_value_t = 1)]
    exit_code: u32,
}

/// The escalation level used when terminating lock holders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Severity {
    /// Ask the holders to shut down cleanly via the Restart Manager (≈ SIGTERM).
    Graceful,
    /// Force the Restart Manager to terminate holders that don't exit cleanly.
    Forced,
    /// Immediately TerminateProcess each holder, bypassing the Restart Manager (≈ SIGKILL).
    Kill,
}

#[cfg(not(windows))]
fn main() {
    eprintln!("reaper only runs on Windows.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    win::run(Cli::parse())
}

#[cfg(windows)]
mod win {
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result};
    use walkdir::WalkDir;
    use windows::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::RestartManager::{
        CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmConsole, RmCritical, RmEndSession, RmExplorer,
        RmForceShutdown, RmGetList, RmMainWindow, RmOtherWindow, RmRegisterResources, RmService,
        RmShutdown, RmStartSession,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    use windows::core::{PCWSTR, PWSTR};

    use crate::{Cli, Severity};

    pub fn run(cli: Cli) -> Result<()> {
        let path = cli
            .path
            .canonicalize()
            .with_context(|| format!("cannot access `{}`", cli.path.display()))?;

        let files = collect_files(&path)?;
        if files.is_empty() {
            println!(
                "Nothing to inspect: `{}` contains no files.",
                path.display()
            );
            return Ok(());
        }

        // Keep the wide-string buffers alive for as long as the PCWSTR pointers
        // into them are used by the Restart Manager.
        let wide: Vec<Vec<u16>> = files.iter().map(|f| to_wide(f.as_os_str())).collect();
        let resources: Vec<PCWSTR> = wide.iter().map(|w| PCWSTR(w.as_ptr())).collect();

        let mut session: u32 = 0;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        check(
            unsafe { RmStartSession(&mut session, None, PWSTR(key.as_mut_ptr())) },
            "RmStartSession",
        )?;

        // Everything between here and RmEndSession runs inside a closure so the
        // session is always torn down, even on the error path.
        let result = (|| -> Result<()> {
            check(
                unsafe { RmRegisterResources(session, Some(&resources), None, None) },
                "RmRegisterResources",
            )?;

            let procs = get_list(session)?;
            if procs.is_empty() {
                println!("No processes are holding a lock on `{}`.", path.display());
                return Ok(());
            }

            println!("Processes holding a lock on `{}`:\n", path.display());
            print_table(&procs);

            if cli.list {
                return Ok(());
            }

            if !cli.yes && !confirm(&cli.severity, procs.len())? {
                println!("Aborted; no processes were terminated.");
                return Ok(());
            }

            println!();
            match cli.severity {
                Severity::Graceful => {
                    check(unsafe { RmShutdown(session, 0, None) }, "RmShutdown")?;
                    println!(
                        "Requested graceful shutdown of {} process(es).",
                        procs.len()
                    );
                }
                Severity::Forced => {
                    check(
                        unsafe { RmShutdown(session, RmForceShutdown.0 as u32, None) },
                        "RmShutdown",
                    )?;
                    println!("Force-shut down {} process(es).", procs.len());
                }
                Severity::Kill => kill_all(&procs, cli.exit_code),
            }
            Ok(())
        })();

        // Best-effort teardown; don't mask the real error if one occurred.
        unsafe {
            let _ = RmEndSession(session);
        }
        result
    }

    /// Collect the files to register: the file itself, or every file under a
    /// directory (the Restart Manager registers files, not directories).
    fn collect_files(path: &Path) -> Result<Vec<PathBuf>> {
        if path.is_dir() {
            let mut files = Vec::new();
            for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    files.push(entry.into_path());
                }
            }
            Ok(files)
        } else {
            Ok(vec![path.to_path_buf()])
        }
    }

    /// Query the Restart Manager for the processes affected by the registered
    /// resources, growing the buffer until it fits.
    fn get_list(session: u32) -> Result<Vec<RM_PROCESS_INFO>> {
        let mut needed: u32 = 0;
        let mut count: u32 = 0;
        let mut reasons: u32 = 0;

        // First probe: discover how many entries exist.
        let err = unsafe { RmGetList(session, &mut needed, &mut count, None, &mut reasons) };
        if err == ERROR_SUCCESS && needed == 0 {
            return Ok(Vec::new());
        }
        if err != ERROR_SUCCESS && err != ERROR_MORE_DATA {
            return Err(win_error("RmGetList", err));
        }

        loop {
            let mut buf = vec![RM_PROCESS_INFO::default(); needed as usize];
            count = needed;
            let ptr = if buf.is_empty() {
                None
            } else {
                Some(buf.as_mut_ptr())
            };
            let err = unsafe { RmGetList(session, &mut needed, &mut count, ptr, &mut reasons) };
            match err {
                ERROR_SUCCESS => {
                    buf.truncate(count as usize);
                    return Ok(buf);
                }
                // The set of holders grew between calls; retry with the new size.
                ERROR_MORE_DATA => continue,
                e => return Err(win_error("RmGetList", e)),
            }
        }
    }

    /// Hard-kill each process via `TerminateProcess`, reporting per-process results.
    fn kill_all(procs: &[RM_PROCESS_INFO], exit_code: u32) {
        for p in procs {
            let pid = p.Process.dwProcessId;
            unsafe {
                match OpenProcess(PROCESS_TERMINATE, false, pid) {
                    Ok(handle) => {
                        match TerminateProcess(handle, exit_code) {
                            Ok(()) => println!("  killed PID {pid} ({})", app_name(p)),
                            Err(e) => eprintln!("  failed to terminate PID {pid}: {e}"),
                        }
                        let _ = CloseHandle(handle);
                    }
                    Err(e) => eprintln!("  cannot open PID {pid} ({}): {e}", app_name(p)),
                }
            }
        }
    }

    /// Print an aligned table of the lock-holding processes.
    fn print_table(procs: &[RM_PROCESS_INFO]) {
        println!("  {:>8}  {:<10}  APPLICATION", "PID", "TYPE");
        println!("  {:>8}  {:<10}  -----------", "--------", "----------");
        for p in procs {
            println!(
                "  {:>8}  {:<10}  {}",
                p.Process.dwProcessId,
                app_type(p),
                app_name(p),
            );
        }
        println!();
    }

    /// Ask the user to confirm the reap, returning `true` to proceed.
    fn confirm(severity: &Severity, count: usize) -> Result<bool> {
        let verb = match severity {
            Severity::Graceful => "gracefully shut down",
            Severity::Forced => "force shut down",
            Severity::Kill => "forcibly kill",
        };
        print!("{verb} {count} process(es)? [y/N] ");
        io::stdout().flush().ok();
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .context("failed to read confirmation")?;
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }

    /// Human-readable label for the Restart Manager application type.
    fn app_type(p: &RM_PROCESS_INFO) -> &'static str {
        match p.ApplicationType {
            t if t == RmMainWindow => "GUI",
            t if t == RmOtherWindow => "GUI (other)",
            t if t == RmService => "Service",
            t if t == RmExplorer => "Explorer",
            t if t == RmConsole => "Console",
            t if t == RmCritical => "Critical",
            _ => "Unknown",
        }
    }

    /// Decode the fixed-width UTF-16 application name into a `String`.
    fn app_name(p: &RM_PROCESS_INFO) -> String {
        let end = p
            .strAppName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(p.strAppName.len());
        String::from_utf16_lossy(&p.strAppName[..end])
    }

    /// Encode an OS string as a NUL-terminated UTF-16 buffer.
    fn to_wide(s: &std::ffi::OsStr) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Turn a non-success `WIN32_ERROR` from a Restart Manager call into an error.
    fn check(err: WIN32_ERROR, what: &str) -> Result<()> {
        if err == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(win_error(what, err))
        }
    }

    fn win_error(what: &str, err: WIN32_ERROR) -> anyhow::Error {
        anyhow::anyhow!(
            "{what} failed: {} (0x{:08X})",
            std::io::Error::from_raw_os_error(err.0 as i32),
            err.0
        )
    }
}
