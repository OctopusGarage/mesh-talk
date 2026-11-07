#!/usr/bin/env python3
"""Linux-only mutation tool owner; acknowledge retirement only after ECHILD.

Tools may create their own process groups. Kernel adoption, rather than a process
name or ancestry scan, gives this single-threaded owner authority over orphans.
"""
import ctypes
import json
import os
import signal
import sys
import time


def main():
    if sys.platform != "linux" or len(sys.argv) < 5 or sys.argv[3] != "--":
        raise RuntimeError("Linux owner requires ACK NONCE -- COMMAND")
    acknowledgement, nonce = sys.argv[1:3]
    if len(nonce) != 64 or any(c not in "0123456789abcdef" for c in nonce):
        raise RuntimeError("invalid retirement nonce")
    if os.path.lexists(acknowledgement) or os.path.lexists(acknowledgement + ".partial"):
        raise RuntimeError("retirement acknowledgement must be new")
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "cannot establish child subreaper")
    flag = ctypes.c_int()
    if libc.prctl(37, ctypes.byref(flag), 0, 0, 0) != 0 or flag.value != 1:
        raise RuntimeError("child subreaper verification failed")
    stopping = False

    def stop(_number, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGCHLD, signal.SIG_DFL)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    # Reject unsupported hosts before admitting tool work.
    if not callable(getattr(signal, "pidfd_send_signal", None)):
        raise RuntimeError("Python pidfd signalling unavailable")
    # Keep enumeration available even if a later pidfd_open encounters EMFILE.
    children = open("/proc/thread-self/children", encoding="ascii")
    leader = leader_fd = leader_status = retirement_started = None
    known = set()

    def candidates():
        children.seek(0)
        return {int(value) for value in children.read().split()}

    def reap():
        nonlocal leader_status
        while True:
            try:
                pid, status = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return True  # ONLY ECHILD proves absence, never proc or PID 0.
            if pid == 0:
                return False
            if pid == leader:
                leader_status = status

    def signal_child(pid, number, pinned=None):
        descriptor = pinned
        try:
            if descriptor is None:
                try:
                    descriptor = os.pidfd_open(pid)
                except ProcessLookupError:
                    return
                except OSError as acquisition_error:
                    # Exceptional acquisition fallback: successful WNOWAIT
                    # proves a DIRECT child. Sole-reaper/default SIGCHLD keeps
                    # that PID unreusable until THIS thread consumes status.
                    try:
                        os.waitid(os.P_PID, pid, os.WEXITED | os.WNOWAIT | os.WNOHANG)
                    except ChildProcessError:
                        raise acquisition_error
                    try:
                        os.kill(pid, number)
                    except ProcessLookupError:
                        pass
                    raise acquisition_error  # Retirement may recover; ACK may not.
            try:
                os.waitid(os.P_PIDFD, descriptor, os.WEXITED | os.WNOWAIT | os.WNOHANG)
            except ChildProcessError:
                return
            try:
                signal.pidfd_send_signal(descriptor, number)
            except ProcessLookupError:
                pass
        finally:
            if descriptor is not None and descriptor != pinned:
                os.close(descriptor)

    try:
        candidates()  # All unsupported-host checks precede tool admission.
        probe = os.pidfd_open(os.getpid())
        try:
            try:
                os.waitid(os.P_PIDFD, probe, os.WEXITED | os.WNOWAIT | os.WNOHANG)
            except ChildProcessError:
                pass  # The operation exists; a process is not its own child.
        finally:
            os.close(probe)
        if stopping:
            raise RuntimeError("cancelled before tool admission")
        # No Popen destructor or SIGCHLD autoreaper may consume child status.
        leader = os.posix_spawnp(sys.argv[4], sys.argv[4:], os.environ, setsid=True)
        known.add(leader)
        leader_fd = os.pidfd_open(leader)
        forwarded = False
        while True:
            if reap():
                if leader_status is None:
                    raise RuntimeError("leader exit status missing")
                exit_code = os.waitstatus_to_exitcode(leader_status)
                exit_code = exit_code if exit_code >= 0 else 128 - exit_code
                with open(acknowledgement + ".partial", "x", encoding="utf8") as stream:
                    json.dump({"schema": 1, "nonce": nonce, "retired": True,
                               "exitCode": exit_code}, stream)
                    stream.write("\n")
                    stream.flush()
                    os.fsync(stream.fileno())
                os.replace(acknowledgement + ".partial", acknowledgement)
                return exit_code
            now = time.monotonic()
            if stopping or leader_status is not None:
                if retirement_started is None:
                    retirement_started = now
                if stopping and not forwarded:
                    # Give Nextest its five-second grace and cargo-mutants time
                    # to restore source before forced, kernel-authorized cleanup.
                    signal_child(leader, signal.SIGTERM, leader_fd)
                    forwarded = True
                if now - retirement_started >= 17:
                    raise RuntimeError("kernel child retirement deadline exceeded")
                if now - retirement_started >= 7:
                    known.update(candidates())
                    for candidate in known:
                        signal_child(candidate, signal.SIGKILL,
                                     leader_fd if candidate == leader else None)
            time.sleep(0.025)
    except Exception:
        # Preserve the FIRST failure and never acknowledge success afterwards.
        # Even acquisition/proc/signal failures must attempt bounded retirement.
        if leader is not None:
            deadline = (retirement_started or time.monotonic()) + 17
            while time.monotonic() < deadline:
                try:
                    if reap():
                        break
                except OSError:
                    pass
                try:
                    known.update(candidates())
                except (OSError, ValueError):
                    pass
                for candidate in known:
                    try:
                        signal_child(candidate, signal.SIGKILL,
                                     leader_fd if candidate == leader else None)
                    except OSError:
                        pass
                time.sleep(0.025)
        raise
    finally:
        if leader_fd is not None:
            try:
                os.close(leader_fd)
            except OSError:
                pass  # Never replace the original retirement failure.
        try:
            children.close()
        except OSError:
            pass


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"mutation process retirement unverified: {error}", file=sys.stderr)
        sys.exit(125)
