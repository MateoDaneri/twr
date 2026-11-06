"""Reproduce pending PTY input loss using only a new private tmux server.

The server is suspended deliberately, then the owned attach client is stopped
with the same SIGHUP/kill fallback used by portable-pty on Unix. This models an
adversarial shutdown timing; it is not a GUI or production-session test. Its
temporary evidence is retained and only the synthetic server is killed.
"""
import array, fcntl, os, pty, signal, struct, subprocess, tempfile, termios, time, tty

root = tempfile.mkdtemp(prefix='twr-pending-pty-', dir='/private/tmp')
socket = root + '/tmux.sock'
ready = root + '/ready'
out = root + '/received'
env = {'PATH': '/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'LANG': 'C.UTF-8'}
base = ['/opt/homebrew/bin/tmux', '-S', socket]
client = None
server_pid = None
stopped = False
master = slave = None

def run(*args):
    return subprocess.run(base + list(args), env=env, check=True, capture_output=True, text=True, timeout=5).stdout.strip()

def wait(label, fn, timeout=3):
    deadline = time.monotonic() + timeout
    while not fn():
        if time.monotonic() >= deadline:
            raise TimeoutError(label)
        time.sleep(0.01)

def available():
    value = array.array('i', [0])
    fcntl.ioctl(slave, termios.FIONREAD, value, True)
    return value[0]

try:
    run('-f', '/dev/null', 'new-session', '-d', '-s', 'probe', '-n', 'test', '/bin/sh')
    run('set-option', '-g', 'status', 'off')
    run('set-option', '-s', 'escape-time', '5000')
    run('set-option', '-t', 'probe', 'destroy-unattached', 'off')
    server_pid = int(run('display-message', '-p', '-t', 'probe', '#{pid}'))
    assert server_pid > 1 and server_pid != os.getpid()
    master, slave = pty.openpty()
    tty.setraw(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
    client = subprocess.Popen(base + ['attach-session', '-t', 'probe'], stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True)
    wait('private client attach', lambda: bool(run('list-clients', '-t', 'probe', '-F', '#{client_pid}')))
    command = "stty raw -echo; printf READY > '%s'; exec /bin/cat > '%s'" % (ready, out)
    run('send-keys', '-t', 'probe:', '-l', command)
    run('send-keys', '-t', 'probe:', 'Enter')
    wait('synthetic cat ready', lambda: os.path.exists(ready) and os.path.exists(out))
    os.kill(server_pid, signal.SIGSTOP)
    stopped = True
    payload = b'SYNTHETIC_PENDING_INPUT'
    assert os.write(master, payload) == len(payload)
    wait('input visible in private slave', lambda: available() == len(payload))
    client.send_signal(signal.SIGHUP)
    try:
        client.wait(timeout=0.25)
    except subprocess.TimeoutExpired:
        client.kill()
        client.wait(timeout=3)
    os.close(master)
    master = None
    os.kill(server_pid, signal.SIGCONT)
    stopped = False
    wait('client disconnected', lambda: not run('list-clients', '-t', 'probe', '-F', '#{client_pid}'))
    run('display-message', '-p', '-t', 'probe', '#{pid}')
    observed = open(out, 'rb').read()
    print('client exited after input was queued; private pane received:', repr(observed))
    assert observed != payload, 'input was delivered; this run does not reproduce loss'
    print('result: close of a private suspended client can lose bytes already in the PTY queue')
finally:
    if stopped and server_pid:
        os.kill(server_pid, signal.SIGCONT)
    try:
        run('kill-server')
    except Exception:
        pass
    if client:
        try:
            client.wait(timeout=3)
        except subprocess.TimeoutExpired:
            client.terminate()
            client.wait(timeout=3)
    if master is not None:
        os.close(master)
    if slave is not None:
        os.close(slave)
    print('synthetic evidence directory:', root)
