"""Demonstrate PTY fd handoff across a synthetic frontend process exit.

Only owned children and a fresh private tmux server are signalled. Temporary
synthetic evidence is retained. This is not a production broker: it omits
authentication, restart recovery, failure-safe handoff, and resource limits.
"""
import array, os, pty, select, signal, socket, struct, subprocess, sys, tempfile, termios, time, tty

if len(sys.argv) > 1 and sys.argv[1] == 'helper':
    s = socket.socket(fileno=int(sys.argv[2]))
    control = int(sys.argv[3])
    msg, ancillary, flags, _ = s.recvmsg(32, socket.CMSG_SPACE(array.array('i').itemsize))
    assert msg == b'PTY' and not (flags & socket.MSG_CTRUNC)
    fds = array.array('i')
    for level, kind, data in ancillary:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            fds.frombytes(data[:len(data) - len(data) % fds.itemsize])
    assert len(fds) == 1
    master = fds[0]
    print('HELD', flush=True)
    while True:
        ready, _, _ = select.select([master, control], [], [], 5)
        if control in ready:
            command = os.read(control, 64)
            if command == b'COMPLETE':
                assert os.write(master, b'[A') == 2
            elif command == b'STOP' or not command:
                break
            else:
                raise RuntimeError('unexpected synthetic control command')
        if master in ready:
            try:
                if not os.read(master, 65536): break
            except OSError:
                break
    os.close(master)
    sys.exit(0)

if len(sys.argv) > 1 and sys.argv[1] == 'frontend':
    s = socket.socket(fileno=int(sys.argv[2]))
    base = [sys.argv[3], '-S', sys.argv[4]]
    env = {'PATH': '/usr/bin:/bin:/opt/homebrew/bin', 'TERM': 'xterm-256color', 'LANG': 'C.UTF-8'}
    master, slave = pty.openpty()
    tty.setraw(slave)
    fcntl = __import__('fcntl')
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
    child = subprocess.Popen(base + ['attach-session', '-t', 'probe'], stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True)
    print('ATTACH', child.pid, flush=True)
    assert sys.stdin.readline() == 'HANDOFF\n'
    data = b'SYNTHETIC_HANDOFF_INPUT\x1b'
    assert os.write(master, data) == len(data)
    pending = array.array('i', [0])
    fcntl.ioctl(slave, termios.FIONREAD, pending, True)
    assert pending[0] == len(data), ('unexpected queue', pending[0])
    assert s.sendmsg([b'PTY'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [master]))]) == 3
    os.close(master)
    os.close(slave)
    s.close()
    print('EXIT', flush=True)
    # Deliberately do not kill or wait for the private attach client: its PTY
    # master was transferred to the helper before this synthetic frontend exits.
    sys.exit(0)

root = tempfile.mkdtemp(prefix='twr-handoff-', dir='/private/tmp')
socket_path = root + '/tmux.sock'
ready_path = root + '/ready'
output_path = root + '/received'
env = {'PATH': '/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'LANG': 'C.UTF-8'}
tmux = '/opt/homebrew/bin/tmux'
base = [tmux, '-S', socket_path]
frontend = helper = None
server_pid = None
stopped = False
control_read, control_write = os.pipe()
a, b = socket.socketpair()

def run(*args):
    return subprocess.run(base + list(args), env=env, check=True, capture_output=True, text=True, timeout=5).stdout.strip()

def wait(label, predicate, timeout=5):
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() >= deadline: raise TimeoutError(label)
        time.sleep(.01)

def line(proc, label):
    if not select.select([proc.stdout], [], [], 5)[0]: raise TimeoutError(label)
    return proc.stdout.readline().decode().strip()

try:
    run('-f', '/dev/null', 'new-session', '-d', '-s', 'probe', '-n', 'test', '/bin/sh')
    run('set-option', '-g', 'status', 'off')
    run('set-option', '-s', 'escape-time', '5000')
    run('set-option', '-t', 'probe', 'destroy-unattached', 'off')
    server_pid = int(run('display-message', '-p', '-t', 'probe', '#{pid}'))
    assert server_pid > 1 and server_pid != os.getpid()
    command = "stty raw -echo; printf READY > '%s'; exec /bin/cat > '%s'" % (ready_path, output_path)
    run('send-keys', '-t', 'probe:', '-l', command)
    run('send-keys', '-t', 'probe:', 'Enter')
    wait('synthetic cat', lambda: os.path.exists(ready_path) and os.path.exists(output_path))
    helper = subprocess.Popen([sys.executable, __file__, 'helper', str(a.fileno()), str(control_read)], pass_fds=(a.fileno(), control_read), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    frontend = subprocess.Popen([sys.executable, __file__, 'frontend', str(b.fileno()), tmux, socket_path], pass_fds=(b.fileno(),), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    a.close(); b.close(); os.close(control_read); control_read = None
    attach = line(frontend, 'frontend started')
    assert attach.startswith('ATTACH ')
    child_pid = int(attach.split()[1])
    wait('attached client', lambda: str(child_pid) in run('list-clients', '-t', 'probe', '-F', '#{client_pid}').splitlines())
    os.kill(server_pid, signal.SIGSTOP); stopped = True
    frontend.stdin.write(b'HANDOFF\n'); frontend.stdin.flush()
    assert line(helper, 'helper acquired master') == 'HELD'
    assert line(frontend, 'frontend handoff') == 'EXIT'
    assert frontend.wait(timeout=3) == 0
    assert helper.poll() is None
    os.kill(server_pid, signal.SIGCONT); stopped = False
    wait('payload delivered after frontend exit', lambda: os.path.getsize(output_path) >= len(b'SYNTHETIC_HANDOFF_INPUT'))
    observed = open(output_path, 'rb').read()
    assert observed == b'SYNTHETIC_HANDOFF_INPUT', observed
    assert str(child_pid) in run('list-clients', '-t', 'probe', '-F', '#{client_pid}').splitlines()
    # The pending Escape is deliberately incomplete. Complete it through the
    # helper only after the synthetic frontend has exited and the server has
    # already processed the plain input.
    os.write(control_write, b'COMPLETE')
    wait('completed arrow after frontend exit', lambda: os.path.getsize(output_path) >= len(b'SYNTHETIC_HANDOFF_INPUT\x1b[A'))
    assert open(output_path, 'rb').read() == b'SYNTHETIC_HANDOFF_INPUT\x1b[A'
    print('helper held PTY after synthetic frontend exited; private pane received queued bytes and a completed split escape sequence')
finally:
    if stopped and server_pid: os.kill(server_pid, signal.SIGCONT)
    try: os.write(control_write, b'STOP')
    except OSError: pass
    if helper:
        try: helper.wait(timeout=3)
        except subprocess.TimeoutExpired: helper.kill(); helper.wait(timeout=3)
    if frontend and frontend.poll() is None:
        frontend.kill(); frontend.wait(timeout=3)
    try: run('kill-server')
    except Exception: pass
    for fd in (control_read, control_write):
        if fd is not None:
            try: os.close(fd)
            except OSError: pass
    for sock in (a,b):
        try: sock.close()
        except OSError: pass
    print('synthetic evidence directory:', root)
