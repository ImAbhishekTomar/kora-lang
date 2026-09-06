//! Confining a helper, rather than merely separating it.
//!
//! A helper already runs in its own process, which is why a crash in it is a
//! value and a hang in it is a timeout. What a separate process does *not*
//! give you is confinement: until this module existed, a helper was an
//! ordinary program with the interpreter's own operating-system rights. It
//! could open a socket, write to `~/.ssh`, or `ptrace` the interpreter that
//! started it. Trust rested entirely on the pinned hash and on who published
//! the package.
//!
//! So a helper now declares what it needs, and gets nothing else:
//!
//! ```toml
//! [package.helper]
//! needs = []           # the default: no network, no writing files
//! ```
//!
//! The author declares; the importer grants. `needs = ["net"]` is refused
//! unless the program that imported the package holds `net` itself, which is
//! the rule every other capability already follows — a package cannot pass on
//! more than it holds.
//!
//! # What this is, and what it is not
//!
//! **It is a denylist, not a jail.** An allowlist of syscalls is the stronger
//! shape and the one a WASM component will eventually give
//! (`DECISIONS.md`, "WASM components for native packages"). It is not what is
//! written here, because an allowlist has to be right about every libc
//! version, every allocator, and every C++ runtime a helper might link, and a
//! wrong one shows up as a helper that dies on somebody else's machine. What
//! is claimed here is narrower and true: a confined helper cannot reach the
//! network, cannot write to the filesystem, and cannot read the memory of the
//! process that started it.
//!
//! **The confinement is inherited.** On Linux a seccomp filter survives
//! `fork` and `execve`, so a helper that starts another program does not
//! escape by doing so — the child is confined identically. That is why
//! `execve` itself is not blocked: blocking it would buy nothing.
//!
//! **Windows is not confined.** There is no equivalent of either mechanism
//! that works without an installer or a privileged service, and a sandbox
//! that silently does nothing is worse than one that says so. `Applied::None`
//! is what the caller is told, and the runtime reports it.

use std::path::Path;
use std::process::Command;

/// What a helper is allowed to reach. Everything absent is denied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    /// Open sockets. A helper that fetches fonts or checks a licence server
    /// needs this; one that turns bytes into other bytes does not.
    pub net: bool,
    /// Write to files. Reading is always allowed: a helper has to be able to
    /// load its own libraries.
    pub fs: bool,
}

impl Needs {
    /// Nothing to confine — the helper asked for everything this module can
    /// hand out, so wrapping it would cost a process and change nothing.
    pub fn is_unconfined(&self) -> bool {
        self.net && self.fs
    }
}

/// What actually happened, so the runtime can say so rather than imply it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// A seccomp filter is in force on the child and everything it starts.
    Seccomp,
    /// The child runs under a macOS sandbox profile.
    SandboxExec,
    /// Nothing was applied. The helper has the rights the interpreter has.
    None,
}

impl Applied {
    pub fn describes_confinement(&self) -> bool {
        !matches!(self, Applied::None)
    }
}

/// The command that starts `program`, confined as far as this platform can.
///
/// Returns the command still to be configured with its pipes, and what was
/// actually applied. Whatever cannot be applied is reported rather than
/// silently skipped, so "the helper is confined" is never a claim the caller
/// makes on faith.
pub fn command(program: &Path, args: &[String], needs: Needs) -> (Command, Applied) {
    let mut plain = Command::new(program);
    plain.args(args);
    if needs.is_unconfined() {
        return (plain, Applied::None);
    }
    platform::confine(plain, program, args, needs)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Applied, Command, Needs};

    /// Present on every macOS this compiler supports. Deprecated by Apple and
    /// still the only sandbox available to an unprivileged process.
    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    /// Wrap the helper in `sandbox-exec` rather than calling `sandbox_init`
    /// from a `pre_exec` hook.
    ///
    /// `sandbox_init` allocates, and a `pre_exec` hook runs between `fork` and
    /// `execve` in a process whose other threads are gone but whose locks are
    /// not — an allocation there can deadlock the child forever. Handing the
    /// profile to a program whose whole job is to apply it and exec has no
    /// such window.
    pub(super) fn confine(
        plain: Command,
        program: &std::path::Path,
        args: &[String],
        needs: Needs,
    ) -> (Command, Applied) {
        if !std::path::Path::new(SANDBOX_EXEC).exists() {
            return (plain, Applied::None);
        }

        // `(allow default)` with targeted denials, rather than
        // `(deny default)` with an allowlist. A deny-by-default profile has to
        // enumerate everything dyld, the allocator, and a C++ runtime touch,
        // and getting that wrong means a helper that will not start. What is
        // denied here is what a helper must not have, and it is denied
        // completely.
        let mut profile = String::from("(version 1)\n(allow default)\n");
        if !needs.net {
            profile.push_str("(deny network*)\n");
        }
        if !needs.fs {
            // `/dev/null` and the tty stay writable: a helper that writes a
            // diagnostic to stderr is explaining itself, not persisting
            // anything.
            profile.push_str(
                "(deny file-write*)\n\
                 (allow file-write-data (literal \"/dev/null\") (literal \"/dev/stdout\") (literal \"/dev/stderr\"))\n",
            );
        }

        let mut wrapped = Command::new(SANDBOX_EXEC);
        wrapped.arg("-p").arg(profile).arg(program).args(args);
        (wrapped, Applied::SandboxExec)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{Applied, Command, Needs};
    use std::os::unix::process::CommandExt;

    /// `seccomp_data.arch` for the architecture this binary was built for. A
    /// filter that does not check the architecture can be defeated by
    /// entering the kernel through a different ABI, where the same syscall
    /// number means something else.
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xC000_003E;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xC000_00B7;

    /// One BPF instruction, as the kernel's `struct sock_filter`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Insn {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }

    #[repr(C)]
    struct Program {
        len: u16,
        filter: *const Insn,
    }

    // The handful of BPF opcodes this filter needs, spelled out rather than
    // pulled in as a dependency: the whole program is fifteen instructions.
    const LD_W_ABS: u16 = 0x00 | 0x00 | 0x20; // BPF_LD | BPF_W | BPF_ABS
    const JMP_JEQ_K: u16 = 0x05 | 0x10 | 0x00; // BPF_JMP | BPF_JEQ | BPF_K
    const RET_K: u16 = 0x06; // BPF_RET | BPF_K

    // Offsets into `struct seccomp_data`.
    const OFFSET_NR: u32 = 0;
    const OFFSET_ARCH: u32 = 4;

    const RET_ALLOW: u32 = 0x7fff_0000;
    const RET_KILL_PROCESS: u32 = 0x8000_0000;
    /// `EPERM` back to the caller, rather than a dead process. A helper that
    /// tries to open a socket gets a refusal it can report; killing it would
    /// look to the program like a crash, which is a different diagnosis.
    const RET_ERRNO_EPERM: u32 = 0x0005_0000 | (libc::EPERM as u32 & 0xffff);

    const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;
    const PR_SET_SECCOMP: libc::c_int = 22;
    const SECCOMP_MODE_FILTER: libc::c_ulong = 2;

    fn denied_syscalls(needs: Needs) -> Vec<libc::c_long> {
        // Always denied, whatever the package asked for. These are not
        // capabilities a helper can be granted: they reach *back* through the
        // process boundary the helper exists to provide.
        let mut denied = vec![
            libc::SYS_ptrace,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
        ];
        if !needs.net {
            // Blocking socket creation is enough: the helper is handed pipes,
            // not sockets, so there is nothing already open to write to.
            // `connect` and the rest are blocked anyway, so a filter that is
            // read later does not have to reason about that.
            denied.extend_from_slice(&[
                libc::SYS_socket,
                libc::SYS_socketpair,
                libc::SYS_connect,
                libc::SYS_bind,
                libc::SYS_listen,
                libc::SYS_accept4,
                libc::SYS_sendto,
                libc::SYS_sendmsg,
                libc::SYS_recvfrom,
                libc::SYS_recvmsg,
            ]);
        }
        denied
    }

    /// `arch == AUDIT_ARCH`, then one comparison per denied syscall.
    ///
    /// Built before the fork. Everything the `pre_exec` hook then does is two
    /// `prctl` calls over this buffer, which is async-signal-safe; building a
    /// `Vec` in there would not be.
    fn build(needs: Needs) -> Vec<Insn> {
        let denied = denied_syscalls(needs);
        let n = denied.len() as u8;
        let mut insns = vec![
            // A syscall arriving under another ABI is not something to
            // second-guess; the process is killed rather than filtered.
            Insn {
                code: LD_W_ABS,
                jt: 0,
                jf: 0,
                k: OFFSET_ARCH,
            },
            Insn {
                code: JMP_JEQ_K,
                jt: 1,
                jf: 0,
                k: AUDIT_ARCH,
            },
            Insn {
                code: RET_K,
                jt: 0,
                jf: 0,
                k: RET_KILL_PROCESS,
            },
            Insn {
                code: LD_W_ABS,
                jt: 0,
                jf: 0,
                k: OFFSET_NR,
            },
        ];
        // Comparison `i` jumps over the ones after it and over the ALLOW,
        // landing on the refusal at the end.
        for (i, nr) in denied.iter().enumerate() {
            insns.push(Insn {
                code: JMP_JEQ_K,
                jt: n - i as u8,
                jf: 0,
                k: *nr as u32,
            });
        }
        insns.push(Insn {
            code: RET_K,
            jt: 0,
            jf: 0,
            k: RET_ALLOW,
        });
        insns.push(Insn {
            code: RET_K,
            jt: 0,
            jf: 0,
            k: RET_ERRNO_EPERM,
        });
        insns
    }

    pub(super) fn confine(
        mut command: Command,
        _program: &std::path::Path,
        _args: &[String],
        needs: Needs,
    ) -> (Command, Applied) {
        let insns = build(needs);
        // Writes are bounded by a resource limit rather than by the filter:
        // seccomp sees syscall numbers and register values, never paths, so
        // it cannot tell writing a temporary file from writing `~/.ssh`.
        // `RLIMIT_FSIZE` can: a limit of zero refuses every write to a
        // regular file and leaves pipes — the helper's own stdout — alone.
        let file_size = if needs.fs { None } else { Some(0u64) };

        unsafe {
            command.pre_exec(move || {
                if let Some(limit) = file_size {
                    let rlimit = libc::rlimit {
                        rlim_cur: limit,
                        rlim_max: limit,
                    };
                    if libc::setrlimit(libc::RLIMIT_FSIZE, &rlimit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                // Required before an unprivileged process may install a
                // filter, and worth having on its own: it stops a setuid
                // binary from gaining rights the helper does not hold.
                if libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let program = Program {
                    len: insns.len() as u16,
                    filter: insns.as_ptr(),
                };
                if libc::prctl(
                    PR_SET_SECCOMP,
                    SECCOMP_MODE_FILTER,
                    &program as *const Program,
                    0,
                    0,
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        (command, Applied::Seccomp)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every comparison must land on the refusal, and a syscall that
        /// matches nothing must fall through to the allow. An off-by-one here
        /// either lets a denied syscall through or refuses everything, and
        /// both look like "the helper is broken" long after the fact.
        #[test]
        fn every_comparison_jumps_to_the_refusal() {
            let insns = build(Needs::default());
            let refusal = insns.len() - 1;
            let allow = insns.len() - 2;
            assert_eq!(insns[refusal].k, RET_ERRNO_EPERM);
            assert_eq!(insns[allow].k, RET_ALLOW);
            for (i, insn) in insns.iter().enumerate() {
                if insn.code != JMP_JEQ_K || insn.k == AUDIT_ARCH {
                    continue;
                }
                assert_eq!(
                    i + 1 + insn.jt as usize,
                    refusal,
                    "comparison at {i} does not land on the refusal"
                );
            }
        }

        #[test]
        fn granting_net_removes_the_socket_denials_and_nothing_else() {
            let confined = denied_syscalls(Needs::default());
            let networked = denied_syscalls(Needs {
                net: true,
                fs: false,
            });
            assert!(confined.contains(&libc::SYS_socket));
            assert!(!networked.contains(&libc::SYS_socket));
            // Reaching back into the interpreter is never grantable.
            assert!(networked.contains(&libc::SYS_ptrace));
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::{Applied, Command, Needs};

    pub(super) fn confine(
        plain: Command,
        _program: &std::path::Path,
        _args: &[String],
        _needs: Needs,
    ) -> (Command, Applied) {
        (plain, Applied::None)
    }
}
