use crate::cover;
use crate::picoc::Picoc;
use crate::utils::{event, Notifier, Waiter};
use byte_slice_cast::*;
use byteorder::WriteBytesExt;
use byteorder::*;
use core::c::iter_trans;
use core::prog::Prog;
use core::target::Target;
use nix::fcntl::{fcntl, FcntlArg};
use nix::poll::{poll, PollFd, PollFlags};
use nix::sys::signal::{kill, Signal};
use nix::unistd::{dup2, fork, ForkResult, Pid};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{Read, Write};
use std::mem;
use std::os::unix::io::AsRawFd;
use std::process::exit;

pub fn fork_exec(p: Prog, t: &Target) -> ExecResult {
    // transfer usefull data
    let (mut rp, mut wp) = os_pipe::pipe()
        .unwrap_or_else(|e| exits!(exitcode::OSERR, "Executor: Fail to pipe : {}", e));
    fcntl(wp.as_raw_fd(), FcntlArg::F_SETPIPE_SZ(1024 * 1024)).unwrap_or_else(|e| {
        exits!(
            exitcode::OSERR,
            "Fail to set pipe size to {} :{}",
            1024 * 1024,
            e
        )
    });

    // collect err msg
    let (mut err_rp, err_wp) = os_pipe::pipe()
        .unwrap_or_else(|e| exits!(exitcode::OSERR, "Executor: Fail to pipe : {}", e));
    // sync data transfer
    let (notifer, waiter) = event();

    match fork() {
        Ok(ForkResult::Child) => {
            drop(rp);
            drop(err_rp);
            drop(notifer);

            dup2(err_wp.as_raw_fd(), 2)
                .unwrap_or_else(|e| exits!(exitcode::OSERR, "Fail to redirect: {}", e));
            dup2(err_wp.as_raw_fd(), 1)
                .unwrap_or_else(|e| exits!(exitcode::OSERR, "Fail to redirect: {}", e));
            drop(err_wp);
            sync_exec(&p, t, &mut wp, waiter);
            exit(exitcode::OK)
        }
        Ok(ForkResult::Parent { child }) => {
            drop(wp);
            drop(err_wp);
            drop(waiter);

            watch(child, &mut rp, &mut err_rp, notifer)
        }
        Err(e) => exits!(exitcode::OSERR, "Executor: Fail to fork: {}", e),
    }
}

fn watch<T: Read + AsRawFd>(
    child: Pid,
    data: &mut T,
    err: &mut T,
    notifer: Notifier,
) -> ExecResult {
    let mut fds = vec![
        PollFd::new(data.as_raw_fd(), PollFlags::POLLIN),
        PollFd::new(err.as_raw_fd(), PollFlags::POLLIN),
    ];
    let mut covs = Vec::new();

    loop {
        match poll(&mut fds, 500) {
            Ok(0) => {
                // timeout
                kill(child, Some(Signal::SIGKILL))
                    .unwrap_or_else(|e| exits!(exitcode::OSERR, "Fail to kill: {}", e));

                return if covs.is_empty() {
                    ExecResult::Err(Error(String::from("Time out")))
                } else {
                    covs.shrink_to_fit();
                    ExecResult::Ok(covs)
                };
            }
            Ok(_) => {
                if let Some(revents) = fds[1].revents() {
                    if !revents.is_empty() {
                        kill(child, Some(Signal::SIGKILL))
                            .unwrap_or_else(|e| exits!(exitcode::OSERR, "Fail to kill: {}", e));

                        let mut err_msg = Vec::new();
                        err.read_to_end(&mut err_msg).unwrap();
                        if covs.is_empty() {
                            return ExecResult::Err(Error(String::from_utf8(err_msg).unwrap()));
                        } else {
                            covs.shrink_to_fit();
                            return ExecResult::Ok(covs);
                        }
                    }
                }

                // Data pipe is ok
                if let Some(revents) = fds[0].revents() {
                    if revents.contains(PollFlags::POLLIN) {
                        let len = data.read_u32::<NativeEndian>().unwrap_or_else(|e| {
                            exits!(exitcode::OSERR, "Fail to read len of covs: {}", e)
                        });
                        let len = len as usize * mem::size_of::<usize>();
                        let mut buf = bytes::BytesMut::with_capacity(len);
                        unsafe {
                            buf.set_len(len);
                        }
                        data.read_exact(&mut buf).unwrap_or_else(|e| {
                            exits!(exitcode::IOERR, "Fail to read len {} of covs: {}", len, e)
                        });
                        notifer.notify();

                        let mut new_cov = Vec::from(buf.as_ref().as_slice_of::<usize>().unwrap());
                        new_cov.shrink_to_fit();
                        covs.push(new_cov);
                    }
                }
            }
            Err(e) => exits!(exitcode::SOFTWARE, "Executor: Fail to poll: {}", e),
        }
    }
}

pub fn sync_exec<T: Write>(p: &Prog, t: &Target, out: &mut T, waiter: Waiter) {
    let mut picoc = Picoc::default();
    let mut handle = cover::open();
    let mut success = false;

    for stmts in iter_trans(p, t) {
        let covs = handle.collect(|| {
            success = picoc.execute(stmts.to_string());
        });
        if success {
            send_covs(covs, out);
            waiter.wait()
        } else {
            exit(exitcode::SOFTWARE)
        }
    }
}

/// Execute prog call by call, send covs in to anything writeable call by call
// pub fn exec<T: Write>(p: &Prog, t: &Target, out: &mut T) {
//     for cov in iter_exec(p, t) {
//         send_covs(cov, out)
//     }
// }
//
// pub fn iter_exec<'a>(p: &'a Prog, t: &'a Target) -> IterExec<'a> {
//     IterExec {
//         pc: Default::default(),
//         handle: cover::open(),
//         p: iter_trans(p, t),
//         finished: false,
//     }
// }
//
// struct IterExec<'a> {
//     pc: Picoc,
//     handle: CovHandle,
//     p: IterTranslate<'a>,
//     finished: bool,
// }
//
// impl<'a> Iterator for IterExec<'a> {
//     type Item = &[usize];
//
//     fn next<'b>(&'b mut self) -> Option<Self::Item> {
//         if self.finished {
//             return None;
//         }
//         if let Some(stmts) = self.p.next() {
//             let covs = self.handle.collect(|| {
//                 let prog = stmts.to_string();
//                 if !self.pc.execute(prog) {
//                     self.finished = true;
//                 }
//             });
//             Some(covs)
//         } else {
//             None
//         }
//     }
// }

fn send_covs<T: Write>(covs: &[usize], out: &mut T) {
    use byte_slice_cast::*;
    assert!(!covs.is_empty());

    out.write_u32::<NativeEndian>(covs.len() as u32)
        .unwrap_or_else(|e| exits!(exitcode::IOERR, "Fail to write len of covs: {}", e));
    out.write_all(covs.as_byte_slice())
        .unwrap_or_else(|e| exits!(exitcode::IOERR, "Fail to write covs: {}", e));
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecResult {
    Ok(Vec<Vec<usize>>),
    Err(Error),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "{}", self.0)
    }
}
